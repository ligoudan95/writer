mod ai;
mod applog;
mod backup;
mod dav;
mod db;
mod docx_import;
mod frontmatter;
mod library;
mod pdf;
mod secrets;
mod sync;

use std::sync::{Arc, Mutex};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use ai::{ChatMsg, ChatStartAck};
use dav::{DavBackend, StorageBackend, TestResult};
use db::AiProvider;
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
    applog::set_library_root(&lib.root);
    applog::info("", &format!("文库已打开：{}", lib.display_root));
    // 启动序列（FR-DOC-01）：磁盘扫描对账（uuid/id 校验）
    let audit_warnings = lib.reconcile_ids();
    if !audit_warnings.is_empty() {
        applog::warn("", &format!("磁盘对账修复 {} 项", audit_warnings.len()));
    }
    // 每日滚动备份（FR-SAV-04）：后台执行，不阻塞启动
    {
        let root = lib.root.clone();
        std::thread::spawn(move || backup::maybe_daily_backup(&root));
    }
    let info = StartupInfo {
        root: lib.display_root.clone(),
        fresh: lib.fresh,
        pending: lib.journal_pending(),
        thirdparty_warnings: sync::detect_thirdparty_sync(&lib.root),
        audit_warnings,
        took_over_stale_lock: lib.took_over_stale_lock,
    };
    let mut guard = state.lib.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(lib);
    Ok(info)
}

// -- 库切换 / 新建 / 最近库（FR-DOC-01 / FR-UI-05 菜单入口） ------------------------

#[tauri::command]
fn library_recent_list() -> CmdResult<Vec<String>> {
    Ok(Library::recent_libraries())
}

fn open_library_into(state: &State<AppState>, lib: Library) -> CmdResult<StartupInfo> {
    applog::set_library_root(&lib.root);
    let audit_warnings = lib.reconcile_ids();
    let info = StartupInfo {
        root: lib.display_root.clone(),
        fresh: lib.fresh,
        pending: lib.journal_pending(),
        thirdparty_warnings: sync::detect_thirdparty_sync(&lib.root),
        audit_warnings,
        took_over_stale_lock: lib.took_over_stale_lock,
    };
    let mut guard = state.lib.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(lib); // 旧库 Drop → 库锁释放
    Ok(info)
}

/// 打开已有库（库切换；前端随后重新初始化文档树/同步/AI 会话）
#[tauri::command]
fn library_open(state: State<AppState>, path: String) -> CmdResult<StartupInfo> {
    let lib = Library::open_at(std::path::Path::new(&path))?;
    lib.remember_recent();
    open_library_into(&state, lib)
}

/// 新建库（指定空目录；含《欢迎使用》）
#[tauri::command]
fn library_create(state: State<AppState>, path: String) -> CmdResult<StartupInfo> {
    let lib = Library::create_at(std::path::Path::new(&path))?;
    lib.remember_recent();
    open_library_into(&state, lib)
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

// -- 回收站面板（FR-DOC-03，M5） ---------------------------------------------------

#[tauri::command]
fn trash_list(state: State<AppState>) -> CmdResult<Vec<library::TrashEntry>> {
    with_lib(&state, |lib| Ok(lib.trash_list()))
}

#[tauri::command]
fn trash_delete(state: State<AppState>, trash_name: String) -> CmdResult<()> {
    with_lib(&state, |lib| lib.trash_delete(&trash_name))
}

#[tauri::command]
fn trash_empty(state: State<AppState>) -> CmdResult<usize> {
    with_lib(&state, |lib| lib.trash_empty())
}

// -- 移动 / 拖拽排序（FR-DOC-02，M5） ----------------------------------------------

#[tauri::command]
fn doc_move(state: State<AppState>, path: String, dest_folder: String) -> CmdResult<String> {
    with_lib(&state, |lib| lib.move_doc(&path, &dest_folder))
}

#[tauri::command]
fn folder_move(state: State<AppState>, path: String, dest_parent: String) -> CmdResult<String> {
    with_lib(&state, |lib| lib.move_folder(&path, &dest_parent))
}

/// 重排序号（步长 10）：ordered_paths 为目标顺序的相对路径列表（同文件夹）
#[tauri::command]
fn doc_reorder(state: State<AppState>, folder: String, ordered_paths: Vec<String>) -> CmdResult<Vec<String>> {
    with_lib(&state, |lib| lib.reorder_docs(&folder, &ordered_paths))
}

// -- 备份 / 诊断（FR-SAV-04 / FR-SET-03，M5） ---------------------------------------

#[tauri::command]
fn backup_now(
    state: State<AppState>,
    include_attachments: bool,
    copy_to: Option<String>,
) -> CmdResult<backup::BackupInfo> {
    let root = lib_root_of(&state)?;
    let info = backup::backup_now(&root, include_attachments, copy_to.as_deref())?;
    applog::info("", &format!("手动备份完成：{}", info.name));
    Ok(info)
}

#[tauri::command]
fn backup_list(state: State<AppState>) -> CmdResult<Vec<backup::BackupInfo>> {
    let root = lib_root_of(&state)?;
    Ok(backup::list_backups(&root))
}

/// 诊断包导出（脱敏；目标路径来自前端系统保存对话框）
#[tauri::command]
fn diagnostics_export(state: State<AppState>, dest: String) -> CmdResult<()> {
    let root = lib_root_of(&state)?;
    let app_cfg = dirs::config_dir().map(|d| d.join("Writer").join("config.json"));
    backup::export_diagnostics(&root, app_cfg.as_deref(), &dest)?;
    applog::info("", "诊断包已导出");
    Ok(())
}

/// 前端日志落同一文件（FR-SET-03：request-id 关联）
#[tauri::command]
fn log_write(level: String, request_id: String, message: String) -> CmdResult<()> {
    applog::write(&level, &request_id, &message);
    Ok(())
}

/// 日志目录路径（"打开日志目录"入口，经 opener reveal）
#[tauri::command]
fn logs_dir() -> CmdResult<String> {
    applog::logs_dir()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| AppError::new("no_library", "尚未打开文库"))
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

    let (lib_root, display_root, settings, password) = {
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
        (lib.root.clone(), lib.display_root.clone(), s, pwd)
    };
    let dir = match direction.as_deref() {
        Some("upload") => Some(Direction::Upload),
        Some("download") => Some(Direction::Download),
        _ => None,
    };

    let rt2 = rt.inner().clone();
    let display_root2 = display_root;
    tauri::async_runtime::spawn_blocking(move || {
        fn tagged(mut v: serde_json::Value, root: &str) -> serde_json::Value {
            if let Some(o) = v.as_object_mut() {
                o.insert("libRoot".into(), serde_json::json!(root));
            }
            v
        }
        // panic 守卫：批次闭包 panic（含引擎异常展开）时 running/phase 仍会复位，
        // 否则同步从此永久拒绝启动（"已有批次进行中"）
        struct BatchGuard(std::sync::Arc<SyncRuntime>);
        impl Drop for BatchGuard {
            fn drop(&mut self) {
                *self.0.phase.lock().unwrap_or_else(|e| e.into_inner()) = "idle".into();
                *self.0.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
            }
        }
        let _guard = BatchGuard(rt2.clone());

        let app2 = app.clone();
        let rt3 = rt2.clone();
        let dr = display_root2.clone();
        let emitter = move |event: &str, payload: serde_json::Value| {
            if event == "sync:state" {
                if let Some(p) = payload.get("phase").and_then(|v| v.as_str()) {
                    *rt3.phase.lock().unwrap_or_else(|e| e.into_inner()) = p.to_string();
                }
            }
            // 库标识：切库后前端按 libRoot 过滤旧批次事件，防污染新库状态
            let _ = app2.emit(event, tagged(payload, &dr));
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
                let v = tagged(serde_json::to_value(&report).unwrap_or_default(), &display_root2);
                *rt2.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
                // 冲突累积（解决后由 sync_conflict_resolve 移除）
                if !conflicts.is_empty() {
                    rt2.conflicts
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend(conflicts.iter().cloned());
                }
                let _ = app.emit(
                    "sync:done",
                    tagged(serde_json::to_value(&report).unwrap_or_default(), &display_root2),
                );
            }
            Ok(SyncOutcome::NeedsDirection { local_docs, remote_docs }) => {
                let _ = app.emit(
                    "sync:needs_direction",
                    tagged(
                        serde_json::json!({ "localDocs": local_docs, "remoteDocs": remote_docs }),
                        &display_root2,
                    ),
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "sync:error",
                    tagged(serde_json::to_value(&e).unwrap_or_default(), &display_root2),
                );
            }
        }
        // BatchGuard Drop 复位 running/phase（正常与 panic 路径统一）
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

// -- AI（FR-AI，M4） ------------------------------------------------------------

fn lib_root_of(state: &State<AppState>) -> Result<std::path::PathBuf, AppError> {
    let guard = state.lib.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .map(|l| l.root.clone())
        .ok_or_else(|| AppError::new("no_library", "尚未打开文库"))
}

#[tauri::command]
fn ai_provider_list(state: State<AppState>) -> CmdResult<Vec<AiProvider>> {
    let root = lib_root_of(&state)?;
    Ok(db::MetaDb::open(&root)?.provider_list())
}

/// 新增/更新 Provider；api_key 经 DPAPI 存 Secrets（FR-AI-01/NFR-08）
#[tauri::command]
fn ai_provider_save(
    state: State<AppState>,
    mut provider: AiProvider,
    api_key: Option<String>,
) -> CmdResult<AiProvider> {
    let root = lib_root_of(&state)?;
    if provider.id.is_empty() {
        provider.id = uuid::Uuid::new_v4().to_string();
    }
    if let Some(key) = &api_key {
        if !key.is_empty() {
            secrets::set(&ai::secrets_key(&provider.id), key)?;
        }
    }
    let db = db::MetaDb::open(&root)?;
    db.provider_upsert(&provider)?;
    Ok(db.provider_get(&provider.id).unwrap_or(provider))
}

#[tauri::command]
fn ai_provider_delete(state: State<AppState>, id: String) -> CmdResult<()> {
    let root = lib_root_of(&state)?;
    let db = db::MetaDb::open(&root)?;
    db.provider_delete(&id);
    let _ = secrets::delete(&ai::secrets_key(&id));
    Ok(())
}

#[tauri::command]
fn ai_provider_set_default(state: State<AppState>, id: Option<String>) -> CmdResult<()> {
    let root = lib_root_of(&state)?;
    let db = db::MetaDb::open(&root)?;
    db.kv_set("default_provider", id.as_deref().unwrap_or(""));
    Ok(())
}

/// 连通性测试（GET /v1/models，不消耗对话 token）；key 留空时回退已存 Key（DPAPI）
#[tauri::command]
async fn ai_connectivity_test(
    state: State<'_, AppState>,
    ai: State<'_, Arc<ai::AiState>>,
    provider_id: Option<String>,
    base_url: String,
    api_key: String,
) -> Result<TestResult, AppError> {
    let key = if api_key.trim().is_empty() {
        match provider_id.as_deref() {
            Some(pid) if !pid.is_empty() => {
                let root = lib_root_of(&state)?;
                let db = db::MetaDb::open(&root)?; // 校验 meta.db 可用（provider 存储于此）
                let name = db.provider_get(pid).map(|p| p.name).unwrap_or_default();
                db.log_request(&name, 0, "test"); // 诊断审计面：测试也是外发（不含内容）
                secrets::get(&ai::secrets_key(pid))?.unwrap_or_default()
            }
            _ => String::new(),
        }
    } else {
        api_key
    };
    match ai::fetch_models(&ai.client, &base_url, &key).await {
        Ok(_) => Ok(TestResult { ok: true, message: "连接成功".into() }),
        Err(e) => Ok(TestResult { ok: false, message: e.message }),
    }
}

#[tauri::command]
async fn ai_models_fetch(
    state: State<'_, AppState>,
    ai: State<'_, Arc<ai::AiState>>,
    provider_id: String,
) -> Result<Vec<String>, AppError> {
    let root = lib_root_of(&state)?;
    let db = db::MetaDb::open(&root)?;
    let p = db
        .provider_get(&provider_id)
        .ok_or_else(|| AppError::new("not_found", "Provider 不存在"))?;
    let key = secrets::get(&ai::secrets_key(&p.id))?.unwrap_or_default();
    db.log_request(&p.name, 0, "models"); // 诊断审计面：模型拉取也是外发（不含内容）
    ai::fetch_models(&ai.client, &p.base_url, &key).await
}

/// 发起 AI 作业（润色/续写/讨论共用）；结果经 ai:chunk/ai:done/ai:error/ai:cancelled 事件推送。
/// 发送事实先行落日志（时间/Provider/字数，不含内容——FR-AI-06）。
#[tauri::command]
async fn ai_chat_start(
    app: AppHandle,
    state: State<'_, AppState>,
    aist: State<'_, Arc<ai::AiState>>,
    kind: String,
    provider_id: String,
    messages: Vec<ChatMsg>,
) -> Result<ChatStartAck, AppError> {
    let root = lib_root_of(&state)?;
    let db = db::MetaDb::open(&root)?;
    let provider = db
        .provider_get(&provider_id)
        .ok_or_else(|| AppError::new("not_found", "Provider 不存在"))?;
    if !provider.enabled {
        return Err(AppError::new("ai_disabled", "该 Provider 已停用"));
    }
    let api_key = secrets::get(&ai::secrets_key(&provider.id))?.unwrap_or_default();
    let chars = ai::total_chars(&messages);
    db.log_request(&provider.name, chars, &kind);

    let job = ai::ChatJob {
        id: ai::new_job_id(),
        kind,
        provider,
        api_key,
        messages,
        lib_root: root,
    };
    let ack = ChatStartAck { job_id: job.id.clone(), chars };
    let aist = aist.inner().clone();
    tauri::async_runtime::spawn(async move {
        ai::run_chat_job(aist, app, job).await;
    });
    Ok(ack)
}

/// 取消：置位标志 → 流被 drop 断连（真实中断，FR-AI-08）
#[tauri::command]
fn ai_job_cancel(aist: State<'_, Arc<ai::AiState>>, job_id: String) -> CmdResult<bool> {
    Ok(ai::cancel_job(&aist, &job_id))
}

// -- AI 对话持久化（FR-AI-04：仅存本地 meta.db，不同步） ---------------------------

#[tauri::command]
fn ai_conversation_ensure(state: State<AppState>, doc_uuid: String, title: String) -> CmdResult<db::AiConversation> {
    let root = lib_root_of(&state)?;
    Ok(db::MetaDb::open(&root)?.conversation_ensure(&doc_uuid, &title)?)
}

#[tauri::command]
fn ai_message_list(state: State<AppState>, conv_id: String) -> CmdResult<Vec<db::AiMessage>> {
    let root = lib_root_of(&state)?;
    Ok(db::MetaDb::open(&root)?.message_list(&conv_id))
}

#[tauri::command]
fn ai_message_append(state: State<AppState>, conv_id: String, role: String, content: String) -> CmdResult<()> {
    let root = lib_root_of(&state)?;
    db::MetaDb::open(&root)?.message_append(&conv_id, &role, &content)?;
    Ok(())
}

/// 重试前回滚末条失败发送（防 DB 重复行）
#[tauri::command]
fn ai_message_delete_last(state: State<AppState>, conv_id: String, role: String) -> CmdResult<()> {
    let root = lib_root_of(&state)?;
    db::MetaDb::open(&root)?.message_delete_last(&conv_id, &role)?;
    Ok(())
}

#[tauri::command]
fn ai_request_log(state: State<AppState>, limit: Option<usize>) -> CmdResult<Vec<db::AiRequestLog>> {
    let root = lib_root_of(&state)?;
    Ok(db::MetaDb::open(&root)?.request_log(limit.unwrap_or(100).min(1000)))
}

#[tauri::command]
fn ai_request_log_clear(state: State<AppState>) -> CmdResult<()> {
    let root = lib_root_of(&state)?;
    db::MetaDb::open(&root)?.request_log_clear();
    Ok(())
}

/// 隐私标记（FR-AI-06）：写 frontmatter，随同步多端生效
#[tauri::command]
fn doc_set_privacy(app: AppHandle, state: State<AppState>, path: String, privacy: bool) -> CmdResult<WriteResult> {
    let r = with_lib(&state, |lib| lib.set_privacy(&path, privacy))?;
    let _ = app.emit(
        "doc:privacy_changed",
        serde_json::json!({ "path": path, "privacy": privacy }),
    );
    Ok(r)
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

/// 导入读取：TXT/MD 走编码检测（BOM/UTF-8/GBK）；DOCX 解析转 Markdown 方言（FR-IMP-01）
#[tauri::command]
fn import_read(path: String) -> CmdResult<ImportedFile> {
    let bytes = std::fs::read(&path)?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "导入文档.md".to_string());
    if path.to_lowercase().ends_with(".docx") {
        let md = docx_import::docx_to_md(&bytes)?;
        let stem = name.trim_end_matches(".docx").trim_end_matches(".DOCX");
        return Ok(ImportedFile { name: format!("{stem}.md"), content: md });
    }
    let content = decode_bytes(&bytes);
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
        // 单实例（FR-SET-04）：二次启动聚焦已有窗口
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }))
        .manage(AppState {
            lib: Mutex::new(None),
        })
        .manage(std::sync::Arc::new(SyncRuntime::default()))
        .manage(std::sync::Arc::new(ai::AiState::new()))
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
            library_recent_list,
            library_open,
            library_create,
            doc_list,
            doc_read,
            doc_write,
            doc_create,
            doc_rename,
            doc_delete,
            doc_move,
            doc_reorder,
            folder_create,
            folder_rename,
            folder_delete,
            folder_move,
            trash_restore,
            trash_list,
            trash_delete,
            trash_empty,
            backup_now,
            backup_list,
            diagnostics_export,
            log_write,
            logs_dir,
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
            ai_provider_list,
            ai_provider_save,
            ai_provider_delete,
            ai_provider_set_default,
            ai_connectivity_test,
            ai_models_fetch,
            ai_chat_start,
            ai_job_cancel,
            ai_conversation_ensure,
            ai_message_list,
            ai_message_append,
            ai_message_delete_last,
            ai_request_log,
            ai_request_log_clear,
            doc_set_privacy,
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
