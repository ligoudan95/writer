mod dav;
mod frontmatter;
mod library;
mod pdf;
mod secrets;
mod sync;

use std::sync::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use dav::{DavBackend, StorageBackend, TestResult};
use library::{
    AppError, CmdResult, DocPayload, Library, SnapshotInfo, StartupInfo, TreeNode, WriteResult,
};
use sync::{ConflictInfo, Direction, SyncDb, SyncLogRow, SyncOutcome, SyncSettings};

/// 全局状态：当前打开的文库（V1 单库）
pub struct AppState {
    pub lib: Mutex<Option<Library>>,
}

/// 同步运行时（批次互斥 + 阶段/结果缓存；命令与后台批次线程共享）
pub struct SyncRuntime {
    pub running: Mutex<bool>,
    pub phase: Mutex<String>,
    pub last: Mutex<Option<serde_json::Value>>,
    pub conflicts: Mutex<Vec<ConflictInfo>>,
}

impl Default for SyncRuntime {
    fn default() -> Self {
        Self {
            running: Mutex::new(false),
            phase: Mutex::new("idle".into()),
            last: Mutex::new(None),
            conflicts: Mutex::new(Vec::new()),
        }
    }
}

fn with_lib<T>(state: &State<AppState>, f: impl FnOnce(&Library) -> CmdResult<T>) -> CmdResult<T> {
    let guard = state.lib.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(lib) => f(lib),
        None => Err(AppError::new("no_library", "尚未打开文库")),
    }
}

/// 启动：读取应用级配置的上次文库；不存在则零配置创建默认文库（含《欢迎使用》）
#[tauri::command]
fn library_startup(state: State<AppState>) -> CmdResult<StartupInfo> {
    let lib = Library::startup()?;
    let info = StartupInfo {
        root: lib.display_root.clone(),
        fresh: lib.fresh,
        pending: lib.journal_pending(),
        thirdparty_warnings: sync::detect_thirdparty_sync(&lib.root),
    };
    let mut guard = state.lib.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(lib);
    Ok(info)
}

#[tauri::command]
fn doc_list(state: State<AppState>) -> CmdResult<Vec<TreeNode>> {
    with_lib(&state, |lib| Ok(lib.list()))
}

#[tauri::command]
fn doc_read(state: State<AppState>, path: String) -> CmdResult<DocPayload> {
    with_lib(&state, |lib| lib.read_doc(&path))
}

/// 保存正文（经 LibraryService 单写者锁 + 原子写 + journal）；
/// expected_hash 为前端持有的 last-read hash，不匹配即返回 conflict（FR-SYN-09 保存护栏）
#[tauri::command]
fn doc_write(
    app: tauri::AppHandle,
    state: State<AppState>,
    path: String,
    body: String,
    expected_hash: String,
    force: bool,
) -> CmdResult<WriteResult> {
    with_lib(&state, |lib| {
        lib.write_doc(&app, &path, &body, &expected_hash, force)
    })
}

#[tauri::command]
fn doc_create(state: State<AppState>, folder: String, title: String) -> CmdResult<TreeNode> {
    with_lib(&state, |lib| lib.create_doc(&folder, &title))
}

/// 库内导入建档（FR-IMP-01）
#[tauri::command]
fn import_doc(
    state: State<AppState>,
    folder: String,
    filename: String,
    content: String,
) -> CmdResult<TreeNode> {
    with_lib(&state, |lib| lib.import_doc(&folder, &filename, &content))
}

#[tauri::command]
fn doc_rename(
    app: tauri::AppHandle,
    state: State<AppState>,
    path: String,
    new_title: String,
) -> CmdResult<WriteResult> {
    with_lib(&state, |lib| lib.rename_doc(&app, &path, &new_title))
}

/// 软删除（文档/文件夹通用）：移入 .writer/trash，返回备份名供撤销
#[tauri::command]
fn doc_delete(state: State<AppState>, path: String) -> CmdResult<String> {
    with_lib(&state, |lib| lib.delete_to_trash(&path))
}

#[tauri::command]
fn trash_restore(state: State<AppState>, trash_name: String, orig_rel: String) -> CmdResult<()> {
    with_lib(&state, |lib| lib.restore_from_trash(&trash_name, &orig_rel))
}

#[tauri::command]
fn folder_create(state: State<AppState>, parent: String, name: String) -> CmdResult<TreeNode> {
    with_lib(&state, |lib| lib.create_folder(&parent, &name))
}

#[tauri::command]
fn folder_rename(state: State<AppState>, path: String, new_name: String) -> CmdResult<String> {
    with_lib(&state, |lib| lib.rename_folder(&path, &new_name))
}

#[tauri::command]
fn folder_delete(state: State<AppState>, path: String) -> CmdResult<String> {
    with_lib(&state, |lib| lib.delete_to_trash(&path))
}

// -- 快照 / 版本历史（FR-SAV-03，M2） ----------------------------------------

#[tauri::command]
fn snapshot_create(state: State<AppState>, path: String, source: String, note: String) -> CmdResult<SnapshotInfo> {
    with_lib(&state, |lib| lib.snapshot_create(&path, &source, &note))
}

#[tauri::command]
fn snapshot_list(state: State<AppState>, path: String) -> CmdResult<Vec<SnapshotInfo>> {
    with_lib(&state, |lib| Ok(lib.snapshot_list(&path)))
}

#[tauri::command]
fn snapshot_read(state: State<AppState>, path: String, ts: u64) -> CmdResult<String> {
    with_lib(&state, |lib| lib.snapshot_read(&path, ts))
}

/// 回滚（FR-SAV-03：回滚前自动存"回滚前"快照，回滚本身可再撤销）
#[tauri::command]
fn snapshot_restore(state: State<AppState>, path: String, ts: u64) -> CmdResult<WriteResult> {
    with_lib(&state, |lib| lib.snapshot_restore(&path, ts))
}

// -- 云同步（FR-SYN，M3） ------------------------------------------------------

#[derive(Serialize, Clone)]
struct SyncConfigView {
    enabled: bool,
    url: String,
    username: String,
    remote_root: String,
    has_password: bool,
    bound: bool,
    auto_sync: bool,
    thirdparty_warnings: Vec<String>,
}

fn sync_view(lib: &Library, s: &SyncSettings) -> SyncConfigView {
    SyncConfigView {
        enabled: s.enabled,
        url: s.dav.url.clone(),
        username: s.dav.username.clone(),
        remote_root: s.dav.remote_root.clone(),
        has_password: s.has_password,
        bound: s.bound,
        auto_sync: s.auto_sync,
        thirdparty_warnings: sync::detect_thirdparty_sync(&lib.root),
    }
}

#[tauri::command]
fn sync_config_get(state: State<AppState>) -> CmdResult<SyncConfigView> {
    with_lib(&state, |lib| {
        let s = sync::load_settings(&lib.root);
        Ok(sync_view(lib, &s))
    })
}

/// 保存配置（FR-SYN-01）：密码经 DPAPI 存 Secrets，绝不入库文件；换 url/根目录重置绑定
#[tauri::command]
fn sync_config_set(
    state: State<AppState>,
    enabled: bool,
    url: String,
    username: String,
    password: Option<String>,
    remote_root: String,
    auto_sync: bool,
) -> CmdResult<SyncConfigView> {
    with_lib(&state, |lib| {
        let mut s = sync::load_settings(&lib.root);
        if s.dav.url != url || s.dav.remote_root != remote_root {
            s.bound = false; // 新的远端目录 = 重新走首次绑定方向确认（FR-SYN-07）
        }
        if let Some(pwd) = &password {
            if !pwd.is_empty() {
                secrets::set(&sync::secrets_key(&lib.root), pwd)?;
                s.has_password = true;
            }
        }
        s.enabled = enabled;
        s.auto_sync = auto_sync;
        s.dav.url = url.trim().to_string();
        s.dav.username = username.trim().to_string();
        s.dav.remote_root = remote_root.trim().trim_matches('/').to_string();
        sync::save_settings(&lib.root, &s)?;
        Ok(sync_view(lib, &s))
    })
}

/// 测试连接（可在保存前调用）：password 为空则用已存凭据
#[tauri::command]
fn sync_test(
    state: State<AppState>,
    url: String,
    username: String,
    password: Option<String>,
    remote_root: String,
) -> CmdResult<TestResult> {
    with_lib(&state, |lib| {
        let pwd = match password {
            Some(p) if !p.is_empty() => p,
            _ => secrets::get(&sync::secrets_key(&lib.root))?.unwrap_or_default(),
        };
        let cfg = dav::DavConfig {
            url,
            username,
            remote_root,
        };
        DavBackend::new(cfg, pwd)?.test_connection()
    })
}

#[derive(Serialize)]
struct SyncRunAck {
    started: bool,
    reason: String,
}

/// 触发一次同步批次（手动 / 保存后自动 / 启动时共用）。
/// 批次在后台线程执行（B2：不阻塞输入路径），状态与结果经 sync:* 事件外显。
#[tauri::command]
async fn sync_now(
    state: State<'_, AppState>,
    rt: State<'_, std::sync::Arc<SyncRuntime>>,
    app: AppHandle,
    direction: Option<String>,
) -> Result<SyncRunAck, AppError> {
    {
        let mut running = rt.running.lock().unwrap_or_else(|e| e.into_inner());
        if *running {
            return Ok(SyncRunAck { started: false, reason: "已有同步批次在进行中".into() });
        }
        *running = true;
    }
    *rt.phase.lock().unwrap_or_else(|e| e.into_inner()) = "scanning".into();

    let (lib_root, settings, password) = {
        let guard = state.lib.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lib) = guard.as_ref() else {
            *rt.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
            return Err(AppError::new("no_library", "尚未打开文库"));
        };
        let s = sync::load_settings(&lib.root);
        if !s.enabled {
            *rt.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
            return Ok(SyncRunAck { started: false, reason: "同步未启用".into() });
        }
        let pwd = secrets::get(&sync::secrets_key(&lib.root))?.unwrap_or_default();
        (lib.root.clone(), s, pwd)
    };
    let dir = match direction.as_deref() {
        Some("upload") => Some(Direction::Upload),
        Some("download") => Some(Direction::Download),
        _ => None,
    };

    let rt2 = rt.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let app2 = app.clone();
        let rt3 = rt2.clone();
        let emitter = move |event: &str, payload: serde_json::Value| {
            if event == "sync:state" {
                if let Some(p) = payload.get("phase").and_then(|v| v.as_str()) {
                    *rt3.phase.lock().unwrap_or_else(|e| e.into_inner()) = p.to_string();
                }
            }
            let _ = app2.emit(event, payload);
        };
        let result = (|| -> Result<SyncOutcome, AppError> {
            let lib = Library::open_existing(&lib_root)?;
            let db = SyncDb::open(&lib_root)?;
            let mut backend = DavBackend::new(settings.dav.clone(), password)?;
            sync::SyncEngine::run(&lib, &db, &mut backend, &settings, dir, &emitter)
        })();
        match result {
            Ok(SyncOutcome::Done(report)) => {
                let conflicts = report.conflicts.clone();
                let v = serde_json::to_value(&report).unwrap_or_default();
                *rt2.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
                // 冲突累积（解决后由 sync_conflict_resolve 移除）
                if !conflicts.is_empty() {
                    rt2.conflicts
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend(conflicts.iter().cloned());
                }
                let _ = app.emit("sync:done", serde_json::to_value(&report).unwrap_or_default());
            }
            Ok(SyncOutcome::NeedsDirection { local_docs, remote_docs }) => {
                let _ = app.emit(
                    "sync:needs_direction",
                    serde_json::json!({ "localDocs": local_docs, "remoteDocs": remote_docs }),
                );
            }
            Err(e) => {
                let _ = app.emit("sync:error", serde_json::to_value(&e).unwrap_or_default());
            }
        }
        // 引擎可能已把 bound 置位并落盘，事件侧无需处理；恢复空闲
        *rt2.phase.lock().unwrap_or_else(|e| e.into_inner()) = "idle".into();
        *rt2.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
    });
    Ok(SyncRunAck { started: true, reason: String::new() })
}

#[derive(Serialize)]
struct SyncStatusView {
    running: bool,
    phase: String,
    last: Option<serde_json::Value>,
    conflicts: Vec<ConflictInfo>,
}

#[tauri::command]
fn sync_status(rt: State<'_, std::sync::Arc<SyncRuntime>>) -> CmdResult<SyncStatusView> {
    Ok(SyncStatusView {
        running: *rt.running.lock().unwrap_or_else(|e| e.into_inner()),
        phase: rt.phase.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        last: rt.last.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        conflicts: rt.conflicts.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    })
}

/// 冲突三动作（FR-SYN-04 v1.7 语义）：keep_remote=false → 保留此版/手动合并后清理；
/// keep_remote=true → 保留彼版（替换前自动快照，uuid 还原为原文档）
#[tauri::command]
fn sync_conflict_resolve(
    state: State<AppState>,
    rt: State<'_, std::sync::Arc<SyncRuntime>>,
    copy_path: String,
    keep_remote: bool,
) -> CmdResult<()> {
    with_lib(&state, |lib| sync::resolve_conflict(lib, &copy_path, keep_remote))?;
    let mut cs = rt.conflicts.lock().unwrap_or_else(|e| e.into_inner());
    *cs = cs.iter().filter(|c| c.copy_path != copy_path).cloned().collect();
    Ok(())
}

#[tauri::command]
fn sync_history(state: State<AppState>, limit: Option<usize>) -> CmdResult<Vec<SyncLogRow>> {
    with_lib(&state, |lib| {
        let db = SyncDb::open(&lib.root)?;
        Ok(db.log_list(limit.unwrap_or(200).min(1000)))
    })
}

/// 逐文档同步状态（FR-SYN-03：文档树图标数据源）
#[tauri::command]
fn sync_doc_states(state: State<AppState>) -> CmdResult<Vec<sync::DocSyncState>> {
    with_lib(&state, |lib| {
        let db = SyncDb::open(&lib.root)?;
        Ok(sync::doc_sync_states(lib, &db))
    })
}

/// 关窗兜底：destroy 后若进程因未决 IPC 残留，前端定时器调用此命令强制退出
#[tauri::command]
fn force_exit(app: tauri::AppHandle) {
    app.exit(0);
}

// -- 导入导出（FR-EXP/FR-IMP，M2；前端不持泛型 fs，走类型化命令） -------------

/// 导出：写入任意路径（用户经系统保存对话框选择）
#[tauri::command]
fn export_write(path: String, data: Vec<u8>) -> CmdResult<()> {
    let p = std::path::Path::new(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    library::atomic_write(p, &data)?;
    Ok(())
}

/// 导入读取：BOM/UTF-8 优先，GBK 兜底（FR-IMP-01 编码自动检测）
#[tauri::command]
fn import_read(path: String) -> CmdResult<ImportedFile> {
    let bytes = std::fs::read(&path)?;
    let content = decode_bytes(&bytes);
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "导入文档.md".to_string());
    Ok(ImportedFile { name, content })
}

#[derive(serde::Serialize)]
struct ImportedFile {
    name: String,
    content: String,
}

fn decode_bytes(bytes: &[u8]) -> String {
    // BOM 检测
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    // 严格 UTF-8 可解则 UTF-8
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    // GBK 兜底（中文 Windows 常见）
    let (decoded, _, had_errors) = encoding_rs::GBK.decode(bytes);
    if had_errors {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        decoded.into_owned()
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            lib: Mutex::new(None),
        })
        .manage(std::sync::Arc::new(SyncRuntime::default()))
        .manage(pdf::PdfState::default())
        .setup(|app| {
            use tauri::Manager;
            // --pdf-selftest <out.pdf>：无 UI 跑通 PDF 管线（分页→COM 打印→落盘）后退出
            if let Some(pos) = std::env::args().position(|a| a == "--pdf-selftest") {
                let out = std::env::args()
                    .nth(pos + 1)
                    .unwrap_or_else(|| ".pdf-selftest.pdf".into());
                if let Some(main) = app.get_webview_window("main") {
                    let _ = main.hide();
                }
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    let r = pdf::export_pdf(&handle, "PDF 管线自检样张", &pdf::selftest_body(), &out);
                    match r {
                        Ok(()) => {
                            println!("PDF_SELFTEST_OK {out}");
                            handle.exit(0);
                        }
                        Err(e) => {
                            println!("PDF_SELFTEST_FAIL {}", e.message);
                            handle.exit(1);
                        }
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library_startup,
            doc_list,
            doc_read,
            doc_write,
            doc_create,
            doc_rename,
            doc_delete,
            trash_restore,
            folder_create,
            folder_rename,
            folder_delete,
            snapshot_create,
            snapshot_list,
            snapshot_read,
            snapshot_restore,
            export_write,
            import_read,
            import_doc,
            sync_config_get,
            sync_config_set,
            sync_test,
            sync_now,
            sync_status,
            sync_conflict_resolve,
            sync_history,
            sync_doc_states,
            pdf::pdf_export,
            pdf::pdf_chamber_pull,
            force_exit
        ])
        .run(tauri::generate_context!())
        .expect("error while running writer");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_utf8_and_boms() {
        assert_eq!(decode_bytes("中文abc".as_bytes()), "中文abc");
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice("带BOM".as_bytes());
        assert_eq!(decode_bytes(&b), "带BOM");
        let text = "宽字符";
        let mut v = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_bytes(&v), text);
    }

    #[test]
    fn decode_gbk_fallback() {
        // "中文" 的 GBK 编码（中=D6D0，文=CED0? 文=0xCEC4）
        assert_eq!(decode_bytes(&[0xD6, 0xD0, 0xCE, 0xC4]), "中文");
        // 纯 ASCII 永远 UTF-8 兼容
        assert_eq!(decode_bytes(b"plain ascii"), "plain ascii");
    }

    #[test]
    fn export_write_creates_nested_file() {
        let dir = std::env::temp_dir().join("writer-export-test");
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("sub").join("hello.txt");
        export_write(p.to_string_lossy().to_string(), b"hello export".to_vec()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello export");
    }
}
