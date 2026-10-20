mod frontmatter;
mod library;
mod pdf;

use std::sync::Mutex;
use tauri::State;

use library::{
    AppError, CmdResult, DocPayload, Library, SnapshotInfo, StartupInfo, TreeNode, WriteResult,
};

/// 全局状态：当前打开的文库（V1 单库）
pub struct AppState {
    pub lib: Mutex<Option<Library>>,
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
