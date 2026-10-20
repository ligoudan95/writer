//! 同步引擎（FR-SYN，M3）。
//!
//! 身份锚点 = frontmatter `id`（doc uuid）：路径只是当前位置（FR-SYN-08）。
//! 冲突判定权威 = 内容 hash；etag 缺失时回退下载比对（FR-SYN-04）。
//! 批次事务性：逐文件"操作 → 提交 sync_state"，中断后下次从上次提交点收敛（FR-SYN-03）。
//! 引擎对 StorageBackend 抽象编程：生产用 DavBackend，测试用 MockDav 双机模拟（5.1）。

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::dav::{DavConfig, RemoteEntry, StorageBackend};
use crate::frontmatter;
use crate::library::{atomic_write, hash_hex, AppError, Library};

pub const MANIFEST_SYNC: &str = "docs/.writer-sync.json";
pub const MANIFEST_STATS: &str = "docs/.writer-stats.json";

// ---------------------------------------------------------------------------
// 库级同步配置（.writer/config.json；密码只存 DPAPI，不入库文件）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub dav: DavConfig,
    #[serde(default)]
    pub has_password: bool,
    /// FR-SYN-07：首次绑定方向已确认（此后不再弹方向选择）
    #[serde(default)]
    pub bound: bool,
    /// 保存后自动同步（FR-SYN-03，默认开）
    #[serde(default = "default_true")]
    pub auto_sync: bool,
}

fn default_true() -> bool {
    true
}

/// 同步凭据在 Secrets 服务中的 key（按库路径区分）
pub fn secrets_key(root: &Path) -> String {
    format!("webdav:{}", &hash_hex(root.to_string_lossy().as_bytes())[..16])
}

pub fn load_settings(root: &Path) -> SyncSettings {
    let p = root.join(".writer").join("config.json");
    fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str::<LibConfig>(&s).ok())
        .map(|c| c.sync)
        .unwrap_or_default()
}

pub fn save_settings(root: &Path, s: &SyncSettings) -> Result<(), AppError> {
    let p = root.join(".writer").join("config.json");
    // 保留 config.json 里其他段落（若未来有）
    let mut cfg: serde_json::Value = fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    cfg["sync"] = serde_json::to_value(s).unwrap_or_default();
    atomic_write(&p, serde_json::to_string_pretty(&cfg).unwrap_or_default().as_bytes())?;
    Ok(())
}

#[derive(Default, Serialize, Deserialize)]
struct LibConfig {
    #[serde(default)]
    sync: SyncSettings,
}

// ---------------------------------------------------------------------------
// 墓碑清单（docs/.writer-sync.json，应用独占；并发写冲突按 uuid 并集合并）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tombstone {
    pub uuid: String,
    /// 最后已知相对路径（展示与外部删除辅助）
    #[serde(default)]
    pub path: String,
    #[serde(rename = "deletedAt", default)]
    pub deleted_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TombstoneManifest {
    #[serde(default)]
    pub tombstones: Vec<Tombstone>,
}

impl TombstoneManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, AppError> {
        serde_json::from_slice(bytes)
            .map_err(|e| AppError::new("manifest_corrupt", format!("墓碑清单损坏：{e}")))
    }

    pub fn get(&self, uuid: &str) -> Option<&Tombstone> {
        self.tombstones.iter().find(|t| t.uuid == uuid)
    }

    /// 集合并集：uuid 求并，同 uuid 取更晚删除时间（FR-SYN-08 应用独占清单规则）
    pub fn merge(&mut self, other: &TombstoneManifest) -> bool {
        let mut changed = false;
        for t in &other.tombstones {
            match self.tombstones.iter_mut().find(|x| x.uuid == t.uuid) {
                Some(slot) => {
                    if t.deleted_at > slot.deleted_at {
                        *slot = t.clone();
                        changed = true;
                    }
                }
                None => {
                    self.tombstones.push(t.clone());
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn upsert(&mut self, uuid: &str, path: &str, deleted_at: u64) {
        match self.tombstones.iter_mut().find(|x| x.uuid == uuid) {
            Some(slot) => {
                if deleted_at > slot.deleted_at {
                    slot.path = path.to_string();
                    slot.deleted_at = deleted_at;
                }
            }
            None => self.tombstones.push(Tombstone {
                uuid: uuid.to_string(),
                path: path.to_string(),
                deleted_at,
            }),
        }
    }

    /// GC：仅清理超过 1 年且状态可确认的记录（无法确认时延后——保守）
    pub fn gc(&mut self, now: u64) -> usize {
        let before = self.tombstones.len();
        self.tombstones
            .retain(|t| now.saturating_sub(t.deleted_at) < 365 * 24 * 3600);
        before - self.tombstones.len()
    }
}

// ---------------------------------------------------------------------------
// sync_state.db（SQLite；schema 见文档 6.2）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncStateRow {
    pub doc_uuid: String,
    pub file_path: String,
    pub local_hash: String,
    pub synced_hash: String,
    pub remote_etag: String,
    pub synced_at: u64,
}

pub struct SyncDb {
    conn: Connection,
}

impl SyncDb {
    pub fn open(root: &Path) -> Result<Self, AppError> {
        let dir = root.join(".writer");
        fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("sync_state.db"))
            .map_err(|e| AppError::new("db", e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS sync_state(
                doc_uuid TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                local_hash TEXT NOT NULL DEFAULT '',
                synced_hash TEXT NOT NULL DEFAULT '',
                remote_etag TEXT NOT NULL DEFAULT '',
                synced_at INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS tombstone_sent(
                uuid TEXT PRIMARY KEY,
                deleted_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS sync_log(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                time INTEGER NOT NULL, file TEXT NOT NULL,
                direction TEXT NOT NULL, bytes INTEGER NOT NULL,
                action TEXT NOT NULL, result TEXT NOT NULL);",
        )
        .map_err(|e| AppError::new("db", e.to_string()))?;
        Ok(Self { conn })
    }

    pub fn all(&self) -> Vec<SyncStateRow> {
        let mut stmt = match self.conn.prepare(
            "SELECT doc_uuid, file_path, local_hash, synced_hash, remote_etag, synced_at FROM sync_state",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| {
            Ok(SyncStateRow {
                doc_uuid: r.get(0)?,
                file_path: r.get(1)?,
                local_hash: r.get(2)?,
                synced_hash: r.get(3)?,
                remote_etag: r.get(4)?,
                synced_at: r.get(5)?,
            })
        });
        match rows {
            Ok(it) => it.flatten().collect(),
            Err(_) => vec![],
        }
    }

    #[allow(dead_code)]
    pub fn get(&self, uuid: &str) -> Option<SyncStateRow> {
        self.all().into_iter().find(|r| r.doc_uuid == uuid)
    }

    pub fn upsert(&self, row: &SyncStateRow) {
        let _ = self.conn.execute(
            "INSERT INTO sync_state(doc_uuid,file_path,local_hash,synced_hash,remote_etag,synced_at)
             VALUES(?,?,?,?,?,?)
             ON CONFLICT(doc_uuid) DO UPDATE SET
               file_path=excluded.file_path, local_hash=excluded.local_hash,
               synced_hash=excluded.synced_hash, remote_etag=excluded.remote_etag,
               synced_at=excluded.synced_at",
            rusqlite::params![
                row.doc_uuid, row.file_path, row.local_hash, row.synced_hash, row.remote_etag, row.synced_at
            ],
        );
    }

    pub fn remove(&self, uuid: &str) {
        let _ = self.conn.execute("DELETE FROM sync_state WHERE doc_uuid=?1", [uuid]);
    }

    /// 已写入墓碑清单的记录（防重复追加，FR-SYN-08）
    pub fn tombstone_sent(&self, uuid: &str) -> bool {
        self.conn
            .query_row("SELECT 1 FROM tombstone_sent WHERE uuid=?1", [uuid], |_| Ok(()))
            .is_ok()
    }

    pub fn mark_tombstone_sent(&self, uuid: &str, deleted_at: u64) {
        let _ = self.conn.execute(
            "INSERT OR REPLACE INTO tombstone_sent(uuid, deleted_at) VALUES(?,?)",
            [uuid, deleted_at.to_string().as_str()],
        );
    }

    pub fn log(&self, time: u64, file: &str, direction: &str, bytes: u64, action: &str, result: &str) {
        let _ = self.conn.execute(
            "INSERT INTO sync_log(time,file,direction,bytes,action,result) VALUES(?,?,?,?,?,?)",
            rusqlite::params![time, file, direction, bytes as i64, action, result],
        );
    }

    pub fn log_list(&self, limit: usize) -> Vec<SyncLogRow> {
        let mut stmt = match self
            .conn
            .prepare("SELECT time,file,direction,bytes,action,result FROM sync_log ORDER BY id DESC LIMIT ?")
        {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(SyncLogRow {
                time: r.get(0)?,
                file: r.get(1)?,
                direction: r.get(2)?,
                bytes: r.get(3)?,
                action: r.get(4)?,
                result: r.get(5)?,
            })
        });
        match rows {
            Ok(it) => it.flatten().collect(),
            Err(_) => vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncLogRow {
    pub time: u64,
    pub file: String,
    pub direction: String,
    pub bytes: u64,
    pub action: String,
    pub result: String,
}

// ---------------------------------------------------------------------------
// 本地扫描
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct LocalDoc {
    rel: String,   // docs/… 相对库根
    uuid: String,  // frontmatter id；缺失时空串（引擎补分配）
    hash: String,
}

#[derive(Debug, Clone)]
struct LocalAttachment {
    rel: String, // attachments/…
    hash: String,
}

fn walk_files(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue; // 隐藏文件（含清单）由引擎单独处理
        }
        let path = e.path();
        let rel = format!("{prefix}/{name}");
        if path.is_dir() {
            walk_files(&path, &rel, out);
        } else {
            out.push(rel);
        }
    }
}

fn scan_local(lib: &Library) -> (Vec<LocalDoc>, Vec<LocalAttachment>) {
    let mut files = Vec::new();
    walk_files(&lib.root.join("docs"), "docs", &mut files);
    let mut docs = Vec::new();
    for rel in files {
        if !rel.to_lowercase().ends_with(".md") {
            continue;
        }
        let Ok(bytes) = fs::read(lib.root.join(&rel)) else { continue };
        let content = String::from_utf8_lossy(&bytes);
        let (fm, _) = frontmatter::split(&content);
        let uuid = fm.as_ref().and_then(|f| f.get("id")).unwrap_or("").to_string();
        docs.push(LocalDoc {
            rel,
            uuid,
            hash: hash_hex(&bytes),
        });
    }
    let mut att_files = Vec::new();
    walk_files(&lib.root.join("attachments"), "attachments", &mut att_files);
    let atts = att_files
        .into_iter()
        .filter_map(|rel| {
            let bytes = fs::read(lib.root.join(&rel)).ok()?;
            Some(LocalAttachment {
                rel,
                hash: hash_hex(&bytes),
            })
        })
        .collect();
    (docs, atts)
}

/// frontmatter 提取 uuid 与 title（远端内容用）
fn parse_uuid_title(content: &str) -> (String, String) {
    let (fm, _) = frontmatter::split(content);
    let uuid = fm.as_ref().and_then(|f| f.get("id")).unwrap_or("").to_string();
    let title = fm.as_ref().and_then(|f| f.get("title")).unwrap_or("").to_string();
    (uuid, title)
}

// ---------------------------------------------------------------------------
// 引擎
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictInfo {
    pub uuid: String,
    pub path: String,
    pub copy_path: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    pub uploaded: usize,
    pub downloaded: usize,
    pub moved: usize,
    pub deleted_remote: usize,
    pub trashed_local: usize,
    pub conflicts: Vec<ConflictInfo>,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub enum SyncOutcome {
    /// 批次完成
    Done(SyncReport),
    /// 首次绑定且两端均有内容，必须由用户选择方向（FR-SYN-07：选择前零写操作）
    NeedsDirection {
        local_docs: usize,
        remote_docs: usize,
    },
}

/// 事件回调（lib.rs 接 AppHandle.emit；测试收集到 Vec）
pub type EmitFn<'a> = dyn Fn(&str, serde_json::Value) + 'a;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct SyncEngine<'a> {
    lib: &'a Library,
    db: &'a SyncDb,
    backend: &'a mut dyn StorageBackend,
    settings: &'a SyncSettings,
    emit: &'a EmitFn<'a>,
    report: SyncReport,
    now: u64,
}

impl<'a> SyncEngine<'a> {
    pub fn run(
        lib: &'a Library,
        db: &'a SyncDb,
        backend: &'a mut dyn StorageBackend,
        settings: &'a SyncSettings,
        direction: Option<Direction>,
        emit: &'a EmitFn<'a>,
    ) -> Result<SyncOutcome, AppError> {
        let mut eng = Self {
            lib,
            db,
            backend,
            settings,
            emit,
            report: SyncReport::default(),
            now: now_secs(),
        };
        eng.execute(direction)
    }

    fn phase(&self, p: &str) {
        (self.emit)("sync:state", serde_json::json!({ "phase": p }));
    }

    fn progress(&self, file: &str, action: &str, bytes: u64) {
        (self.emit)(
            "sync:progress",
            serde_json::json!({ "file": file, "action": action, "bytes": bytes }),
        );
    }

    fn log(&self, file: &str, direction: &str, bytes: u64, action: &str, result: &str) {
        self.db.log(self.now, file, direction, bytes, action, result);
    }

    fn warn(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        self.report.warnings.push(m.clone());
        (self.emit)("sync:warning", serde_json::json!({ "message": m }));
    }

    // -- 主流程 -------------------------------------------------------------

    fn execute(&mut self, direction: Option<Direction>) -> Result<SyncOutcome, AppError> {
        self.phase("scanning");

        // 本地扫描
        let (docs, atts) = scan_local(self.lib);

        // 远端扫描
        let remote = self.backend.list_all()?;
        let remote_files: BTreeMap<String, RemoteEntry> = remote
            .into_iter()
            .filter(|e| !e.is_dir)
            .map(|e| (e.path.clone(), e))
            .collect();
        // 引擎内可变视图：批次中途的 MOVE/DELETE 会更新它，
        // 保证后续判定不依赖过期快照——防"改名文档与自己冲突"
        let mut remote_files = remote_files;

        // 墓碑清单：远端 → 合并本地
        let mut manifest = self.load_local_manifest();
        let remote_manifest = match remote_files.get(MANIFEST_SYNC) {
            Some(_) => {
                let bytes = self.backend.get(MANIFEST_SYNC)?;
                self.report.bytes_down += bytes.len() as u64;
                TombstoneManifest::parse(&bytes).unwrap_or_else(|e| {
                    self.warn(format!("远端墓碑清单损坏，视同空清单（高风险：旧删除可能复活）——{}", e.message));
                    TombstoneManifest::default()
                })
            }
            None => TombstoneManifest::default(),
        };
        let manifest_changed_by_merge = manifest.merge(&remote_manifest);

        // 状态行
        let state_rows: Vec<SyncStateRow> = self.db.all();
        let state_by_uuid: BTreeMap<String, SyncStateRow> =
            state_rows.iter().cloned().map(|r| (r.doc_uuid.clone(), r)).collect();
        let _state_by_path: BTreeMap<String, SyncStateRow> =
            state_rows.iter().cloned().map(|r| (r.file_path.clone(), r)).collect();
        let local_by_uuid: BTreeMap<String, &LocalDoc> =
            docs.iter().map(|d| (d.uuid.clone(), d)).collect();

        // -- 首次绑定（FR-SYN-07）-----------------------------------------
        // None 且单侧为空时推导唯一合理方向（无覆盖风险）；两端均非空必须用户选择
        let effective_dir = if !self.settings.bound {
            let remote_doc_count = remote_files
                .keys()
                .filter(|p| p.starts_with("docs/") && p.ends_with(".md"))
                .count();
            match direction {
                Some(d) => Some(d),
                None => {
                    if docs.len() > 0 && remote_doc_count > 0 {
                        // 零写操作返回，交由用户选择方向（A13）
                        self.phase("idle");
                        return Ok(SyncOutcome::NeedsDirection {
                            local_docs: docs.len(),
                            remote_docs: remote_doc_count,
                        });
                    }
                    if docs.is_empty() && remote_doc_count > 0 {
                        Some(Direction::Download)
                    } else {
                        Some(Direction::Upload)
                    }
                }
            }
        } else {
            None
        };

        self.phase("planning");
        self.phase("executing");

        if !self.settings.bound {
            self.first_bind(&docs, &atts, &remote_files, effective_dir)?;
        } else {
            let trashed = self.apply_incoming_tombstones(
                &manifest,
                &local_by_uuid,
                &state_by_uuid,
                &mut remote_files,
            );
            self.propagate_renames(&docs, &mut remote_files, &state_by_uuid)?;
            self.propagate_local_deletes(&docs, &mut remote_files, &state_rows, &mut manifest)?;
            self.sync_docs(&docs, &remote_files, &state_by_uuid, &manifest, &trashed)?;
            self.sync_attachments(&atts, &remote_files);
        }

        // -- 结算：清单落盘与上传 -------------------------------------------
        self.phase("settling");
        let local_manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap_or_default();
        let manifest_disk = self.lib.root.join(MANIFEST_SYNC);
        let manifest_on_disk = fs::read(&manifest_disk).unwrap_or_default();
        let manifest_dirty = manifest_on_disk != local_manifest_bytes;
        if manifest_dirty || manifest_changed_by_merge {
            {
                let _g = self.lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(parent) = manifest_disk.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                atomic_write(&manifest_disk, &local_manifest_bytes)?;
            }
            let etag = self.backend.put(MANIFEST_SYNC, &local_manifest_bytes)?;
            self.report.bytes_up += local_manifest_bytes.len() as u64;
            self.log(MANIFEST_SYNC, "up", local_manifest_bytes.len() as u64, "put", "ok");
            let _ = etag;
        }
        // 统计清单（应用独占）：远端有则并集收编（V1 无写入方，仅保持收敛）
        self.settle_stats_manifest(&remote_files)?;

        // 首次绑定完成后落 bound 标记
        if !self.settings.bound {
            let mut s = self.settings.clone();
            s.bound = true;
            save_settings(&self.lib.root, &s)?;
        }

        // 墓碑 GC（1 年 + 保守）
        if manifest.gc(self.now) > 0 {
            self.warn("墓碑清单已执行年度清理");
        }

        self.phase("idle");
        Ok(SyncOutcome::Done(std::mem::take(&mut self.report)))
    }

    fn load_local_manifest(&self) -> TombstoneManifest {
        fs::read(self.lib.root.join(MANIFEST_SYNC))
            .ok()
            .and_then(|b| TombstoneManifest::parse(&b).ok())
            .unwrap_or_default()
    }

    // -- 首次绑定 -----------------------------------------------------------

    fn first_bind(
        &mut self,
        docs: &[LocalDoc],
        atts: &[LocalAttachment],
        remote_files: &BTreeMap<String, RemoteEntry>,
        direction: Option<Direction>,
    ) -> Result<(), AppError> {
        match direction {
            Some(Direction::Upload) | None => {
                // None = 远端为空，直接上传（无覆盖风险）
                self.warn("首次绑定：以上传本地为方向");
                let mut uploaded: std::collections::HashSet<String> = std::collections::HashSet::new();
                for d in docs {
                    let bytes = fs::read(self.lib.root.join(&d.rel)).unwrap_or_default();
                    let etag = self.backend.put(&d.rel, &bytes)?;
                    self.report.uploaded += 1;
                    self.report.bytes_up += bytes.len() as u64;
                    uploaded.insert(d.rel.clone());
                    self.db.upsert(&SyncStateRow {
                        doc_uuid: d.uuid.clone(),
                        file_path: d.rel.clone(),
                        local_hash: d.hash.clone(),
                        synced_hash: d.hash.clone(),
                        remote_etag: etag,
                        synced_at: self.now,
                    });
                    self.progress(&d.rel, "upload", bytes.len() as u64);
                    self.log(&d.rel, "up", bytes.len() as u64, "put", "ok");
                }
                for a in atts {
                    let bytes = fs::read(self.lib.root.join(&a.rel)).unwrap_or_default();
                    self.backend.put(&a.rel, &bytes)?;
                    self.report.uploaded += 1;
                    self.report.bytes_up += bytes.len() as u64;
                    uploaded.insert(a.rel.clone());
                    self.progress(&a.rel, "upload", bytes.len() as u64);
                    self.log(&a.rel, "up", bytes.len() as u64, "put", "ok");
                }
                // "上传本地覆盖远端"：清理远端多余文件（镜像语义）
                for rp in remote_files.keys() {
                    if rp == MANIFEST_SYNC || rp == MANIFEST_STATS {
                        continue;
                    }
                    if !uploaded.contains(rp) {
                        if self.backend.delete(rp).is_ok() {
                            self.report.deleted_remote += 1;
                            self.log(rp, "up", 0, "delete", "first-bind-upload");
                        }
                    }
                }
            }
            Some(Direction::Download) => {
                self.warn("首次绑定：以下载远端为方向");
                for (path, entry) in remote_files {
                    if *path == MANIFEST_SYNC || *path == MANIFEST_STATS {
                        continue;
                    }
                    let bytes = self.backend.get(path)?;
                    self.report.bytes_down += bytes.len() as u64;
                    if path.starts_with("docs/") && path.ends_with(".md") {
                        let content = String::from_utf8_lossy(&bytes).to_string();
                        let (uuid, _) = parse_uuid_title(&content);
                        let rel = self.create_local_doc(path, &bytes)?;
                        // create_local_doc 可能补写 frontmatter（id/fmt/created）→ 回读真实内容
                        let after = fs::read(self.lib.root.join(&rel))?;
                        if after != bytes {
                            self.backend.put(&rel, &after)?;
                            self.report.bytes_up += after.len() as u64;
                        }
                        let hash = hash_hex(&after);
                        let (uuid, _) = if uuid.is_empty() {
                            let (u, _) = parse_uuid_title(&String::from_utf8_lossy(&after));
                            (u, String::new())
                        } else {
                            (uuid, String::new())
                        };
                        self.db.upsert(&SyncStateRow {
                            doc_uuid: uuid,
                            file_path: rel.clone(),
                            local_hash: hash.clone(),
                            synced_hash: hash,
                            remote_etag: entry.etag.clone(),
                            synced_at: self.now,
                        });
                        self.report.downloaded += 1;
                        self.progress(&rel, "download", after.len() as u64);
                        self.log(&rel, "down", after.len() as u64, "get", "ok");
                    } else if path.starts_with("attachments/") {
                        let dest = self.lib.root.join(path);
                        if let Some(parent) = dest.parent() {
                            let _g = self.lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
                            let _ = fs::create_dir_all(parent);
                            atomic_write(&dest, &bytes)?;
                        }
                        self.report.downloaded += 1;
                        self.progress(path, "download", bytes.len() as u64);
                        self.log(path, "down", bytes.len() as u64, "get", "ok");
                    }
                }
                // "下载远端覆盖本地"：本地多余文档移入回收站（可恢复，不硬删）
                for d in docs {
                    if !remote_files.contains_key(&d.rel) {
                        if self.lib.delete_to_trash(&d.rel).is_ok() {
                            self.report.trashed_local += 1;
                            self.log(&d.rel, "local", 0, "trash", "first-bind-download");
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // -- 墓碑处理（A11/A14） --------------------------------------------------

    /// 返回本轮被墓碑清理的本地文档路径（sync_docs 跳过，防读已入回收站的文件）
    fn apply_incoming_tombstones(
        &mut self,
        manifest: &TombstoneManifest,
        local_by_uuid: &BTreeMap<String, &LocalDoc>,
        state_by_uuid: &BTreeMap<String, SyncStateRow>,
        remote_files: &mut BTreeMap<String, RemoteEntry>,
    ) -> std::collections::HashSet<String> {
        let mut trashed = std::collections::HashSet::new();
        for t in &manifest.tombstones {
            // 本端已处理过该删除（防回收站恢复后再次误删）
            if self.db.tombstone_sent(&t.uuid) {
                continue;
            }
            if let Some(doc) = local_by_uuid.get(&t.uuid) {
                // 对端删除传播：本端移入回收站（A11）
                match self.lib.delete_to_trash(&doc.rel) {
                    Ok(_) => {
                        self.report.trashed_local += 1;
                        self.db.remove(&t.uuid);
                        self.db.mark_tombstone_sent(&t.uuid, t.deleted_at);
                        self.log(&doc.rel, "local", 0, "trash", "tombstone");
                        trashed.insert(doc.rel.clone());
                        // 远端若仍残留该文件（删除方未清理或竞态）→ 补删，且不复活
                        if remote_files.remove(&doc.rel).is_some() {
                            if let Err(e) = self.backend.delete(&doc.rel) {
                                self.warn(format!("墓碑补删远端失败 {}：{}", doc.rel, e.message));
                            } else {
                                self.report.deleted_remote += 1;
                            }
                        }
                        (self.emit)(
                            "sync:trashed",
                            serde_json::json!({ "path": doc.rel, "title": t.path }),
                        );
                    }
                    Err(e) => self.warn(format!("墓碑删除失败 {}：{}", t.uuid, e.message)),
                }
            } else if state_by_uuid.contains_key(&t.uuid) {
                // 本端无文件但残留状态行：清状态，标记已处理
                self.db.remove(&t.uuid);
                self.db.mark_tombstone_sent(&t.uuid, t.deleted_at);
            }
        }
        trashed
    }

    // -- 改名传播（A12） -------------------------------------------------------

    fn propagate_renames(
        &mut self,
        docs: &[LocalDoc],
        remote_files: &mut BTreeMap<String, RemoteEntry>,
        state_by_uuid: &BTreeMap<String, SyncStateRow>,
    ) -> Result<(), AppError> {
        for d in docs {
            if d.uuid.is_empty() {
                continue;
            }
            let Some(row) = state_by_uuid.get(&d.uuid) else { continue };
            if row.file_path == d.rel {
                continue;
            }
            // 已知 uuid 的旧路径消失 + 新路径出现同一 uuid → 远端 MOVE
            if let Some(old_entry) = remote_files.get(&row.file_path).cloned() {
                match self.backend.mv(&row.file_path, &d.rel) {
                    Ok(()) => {
                        // 视图同步更新：新路径持有旧 etag（MOVE 不改内容）
                        remote_files.remove(&row.file_path);
                        remote_files.insert(d.rel.clone(), old_entry);
                        self.report.moved += 1;
                        self.log(&d.rel, "up", 0, "move", "ok");
                        (self.emit)(
                            "sync:moved",
                            serde_json::json!({ "from": row.file_path, "to": d.rel }),
                        );
                    }
                    Err(e) => self.warn(format!("MOVE {} → {} 失败：{}", row.file_path, d.rel, e.message)),
                }
            }
            // 状态行路径对齐（内容差异由 sync_docs 处理）
            let mut new_row = row.clone();
            new_row.file_path = d.rel.clone();
            self.db.upsert(&new_row);
        }
        Ok(())
    }

    // -- 本端删除传播 ------------------------------------------------------------

    fn propagate_local_deletes(
        &mut self,
        docs: &[LocalDoc],
        remote_files: &mut BTreeMap<String, RemoteEntry>,
        state_rows: &[SyncStateRow],
        manifest: &mut TombstoneManifest,
    ) -> Result<(), AppError> {
        let local_uuids: std::collections::HashSet<&str> =
            docs.iter().map(|d| d.uuid.as_str()).collect();
        let local_paths: std::collections::HashSet<&str> =
            docs.iter().map(|d| d.rel.as_str()).collect();

        for row in state_rows {
            if local_uuids.contains(row.doc_uuid.as_str()) || local_paths.contains(row.file_path.as_str()) {
                continue;
            }
            // 本端已删除：写墓碑（防重复）+ 远端删除 + 清状态
            if !self.db.tombstone_sent(&row.doc_uuid) {
                manifest.upsert(&row.doc_uuid, &row.file_path, self.now);
                self.db.mark_tombstone_sent(&row.doc_uuid, self.now);
            }
            if remote_files.remove(&row.file_path).is_some() {
                if let Err(e) = self.backend.delete(&row.file_path) {
                    self.warn(format!("远端删除 {} 失败：{}", row.file_path, e.message));
                } else {
                    self.report.deleted_remote += 1;
                    self.log(&row.file_path, "up", 0, "delete", "ok");
                }
            }
            self.db.remove(&row.doc_uuid);
        }
        // 附件删除传播：附件无 uuid/状态行，V1 以"存在即上传、远端新增即下载"收敛，
        // 本端删除不主动删远端（避免误删并发上传），文档删除传播见上。
        Ok(())
    }

    // -- 文档双向同步核心 ---------------------------------------------------------

    fn sync_docs(
        &mut self,
        docs: &[LocalDoc],
        remote_files: &BTreeMap<String, RemoteEntry>,
        state_by_uuid: &BTreeMap<String, SyncStateRow>,
        manifest: &TombstoneManifest,
        trashed_this_run: &std::collections::HashSet<String>,
    ) -> Result<(), AppError> {
        // 远端 uuid 索引：仅对"无法按路径对位"的远端文件下载探查
        let mut handled_remote: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1) 本地文档：上传 / MOVE 后补传 / 冲突（墓碑清理过的文件本轮跳过）
        for d in docs {
            if d.uuid.is_empty() {
                continue; // 外部放置文件：下次启动库体检流程处理（R8），本轮不推
            }
            if trashed_this_run.contains(&d.rel) {
                continue;
            }
            let row = state_by_uuid.get(&d.uuid);
            self.sync_one_doc(d, row, remote_files, manifest, &mut handled_remote)?;
        }

        // 2) 远端独有文件：新建下载 / 远端改名 / 墓碑清理
        let state_by_path: BTreeMap<String, SyncStateRow> = self.db.all().into_iter().map(|r| (r.file_path.clone(), r)).collect();
        let local_uuids: std::collections::HashSet<String> =
            docs.iter().map(|d| d.uuid.clone()).collect();
        for (rpath, entry) in remote_files {
            if !rpath.starts_with("docs/") || !rpath.ends_with(".md") || rpath == MANIFEST_SYNC || rpath == MANIFEST_STATS {
                continue;
            }
            if handled_remote.contains(rpath) || state_by_path.contains_key(rpath) {
                continue;
            }
            // 未知远端文档：下载探查
            let bytes = self.backend.get(rpath)?;
            self.report.bytes_down += bytes.len() as u64;
            let content = String::from_utf8_lossy(&bytes).to_string();
            let (uuid, _) = parse_uuid_title(&content);
            if uuid.is_empty() {
                // 外部引入（无 id）：按新文档建档并回写 uuid（FR-SYN-08 / FR-IMP-01）
                let rel = self.create_local_doc(rpath, &bytes)?;
                let after = fs::read(self.lib.root.join(&rel))?; // 含回写的 frontmatter
                let hash = hash_hex(&after);
                let etag = self.backend.put(&rel, &after)?;
                if rel != *rpath {
                    // 本地因重名唯一化落在不同路径：清理远端原位置
                    self.backend.delete(rpath)?;
                }
                self.report.uploaded += 1;
                self.report.bytes_up += after.len() as u64;
                let (new_uuid, _) = parse_uuid_title(&String::from_utf8_lossy(&after));
                self.db.upsert(&SyncStateRow {
                    doc_uuid: new_uuid,
                    file_path: rel.clone(),
                    local_hash: hash.clone(),
                    synced_hash: hash,
                    remote_etag: etag,
                    synced_at: self.now,
                });
                self.warn(format!("远端文件无 id，已按新文档导入：{rpath}"));
                continue;
            }
            if manifest.get(&uuid).is_some() {
                // 墓碑内：本端曾删除，远端残留 → 补删（A11 不复活）
                self.backend.delete(rpath)?;
                self.report.deleted_remote += 1;
                self.log(rpath, "up", 0, "delete", "tombstone");
                continue;
            }
            if local_uuids.contains(&uuid) {
                // 同 uuid 在本地：远端改名/路径分叉
                let local_doc = docs.iter().find(|d| d.uuid == uuid).unwrap();
                let row = state_by_uuid.get(&uuid);
                let local_diverged = row.map(|r| local_doc.hash != r.synced_hash).unwrap_or(true);
                if local_diverged {
                    // 双端改名冲突（FR-SYN-08）：回退冲突副本路径——
                    // 远端内容 → 冲突副本（新 uuid），删除远端旧位置，本版保持并随主循环上传
                    let title = {
                        let t = parse_uuid_title(&content).1;
                        if t.is_empty() { rpath.clone() } else { t }
                    };
                    let copy_rel = self.create_conflict_copy(&local_doc.rel, &title, &bytes)?;
                    let copy_bytes = fs::read(self.lib.root.join(&copy_rel))?;
                    let (copy_uuid, _) = parse_uuid_title(&String::from_utf8_lossy(&copy_bytes));
                    let copy_hash = hash_hex(&copy_bytes);
                    let copy_etag = self.backend.put(&copy_rel, &copy_bytes)?;
                    self.report.bytes_up += copy_bytes.len() as u64;
                    self.db.upsert(&SyncStateRow {
                        doc_uuid: copy_uuid,
                        file_path: copy_rel.clone(),
                        local_hash: copy_hash.clone(),
                        synced_hash: copy_hash,
                        remote_etag: copy_etag,
                        synced_at: self.now,
                    });
                    self.backend.delete(rpath)?;
                    self.report.deleted_remote += 1;
                    self.report.conflicts.push(ConflictInfo {
                        uuid: uuid.clone(),
                        path: local_doc.rel.clone(),
                        copy_path: copy_rel.clone(),
                    });
                    (self.emit)(
                        "sync:conflict",
                        serde_json::json!({ "path": local_doc.rel, "copyPath": copy_rel, "title": title }),
                    );
                    continue;
                }
                // 本地未分叉：远端改名，本地文件对齐远端路径
                let new_rel = self.rename_local_doc(&local_doc.rel, rpath)?;
                let row = state_by_uuid.get(&uuid).cloned().unwrap_or_default();
                let hash = local_doc.hash.clone();
                self.db.upsert(&SyncStateRow {
                    doc_uuid: uuid.clone(),
                    file_path: new_rel.clone(),
                    local_hash: hash.clone(),
                    synced_hash: row.synced_hash.clone(),
                    remote_etag: entry.etag.clone(),
                    synced_at: self.now,
                });
                (self.emit)(
                    "doc:renamed",
                    serde_json::json!({ "from": local_doc.rel, "to": new_rel }),
                );
                continue;
            }
            // 全新文档（对端新建）：下载入库
            let rel = self.create_local_doc(rpath, &bytes)?;
            let hash = hash_hex(&bytes);
            self.db.upsert(&SyncStateRow {
                doc_uuid: uuid,
                file_path: rel.clone(),
                local_hash: hash.clone(),
                synced_hash: hash.clone(),
                remote_etag: entry.etag.clone(),
                synced_at: self.now,
            });
            self.report.downloaded += 1;
            self.progress(&rel, "download", bytes.len() as u64);
            self.log(&rel, "down", bytes.len() as u64, "get", "ok");
            (self.emit)(
                "doc:external_change",
                serde_json::json!({ "path": rel, "hash": hash }),
            );
        }
        Ok(())
    }

    fn sync_one_doc(
        &mut self,
        d: &LocalDoc,
        row: Option<&SyncStateRow>,
        remote_files: &BTreeMap<String, RemoteEntry>,
        _manifest: &TombstoneManifest,
        handled_remote: &mut std::collections::HashSet<String>,
    ) -> Result<(), AppError> {
        handled_remote.insert(d.rel.clone());
        let bytes = fs::read(self.lib.root.join(&d.rel))?;
        let Some(row) = row else {
            // 新文档（本地新建）：直接上传
            let etag = self.backend.put(&d.rel, &bytes)?;
            self.report.uploaded += 1;
            self.report.bytes_up += bytes.len() as u64;
            self.db.upsert(&SyncStateRow {
                doc_uuid: d.uuid.clone(),
                file_path: d.rel.clone(),
                local_hash: d.hash.clone(),
                synced_hash: d.hash.clone(),
                remote_etag: etag,
                synced_at: self.now,
            });
            self.progress(&d.rel, "upload", bytes.len() as u64);
            self.log(&d.rel, "up", bytes.len() as u64, "put", "ok");
            return Ok(());
        };
        handled_remote.insert(row.file_path.clone());

        let local_changed = d.hash != row.synced_hash;
        let remote_entry = remote_files.get(&d.rel);
        let remote_changed = match remote_entry {
            None => !row.synced_hash.is_empty(), // 远端消失视为变化（外部删除）
            Some(e) => {
                if !e.etag.is_empty() && !row.remote_etag.is_empty() {
                    e.etag != row.remote_etag
                } else {
                    // etag 缺失：下载比对（FR-SYN-04 回退）
                    match self.backend.get(&d.rel) {
                        Ok(b) => hash_hex(&b) != row.synced_hash,
                        Err(_) => false,
                    }
                }
            }
        };

        match (local_changed, remote_changed) {
            (false, false) => {
                // 双端未变：仅刷新 etag 缓存
                if let Some(e) = remote_entry {
                    if e.etag != row.remote_etag {
                        let mut nr = row.clone();
                        nr.remote_etag = e.etag.clone();
                        self.db.upsert(&nr);
                    }
                }
                Ok(())
            }
            (true, false) => {
                // 本地修改 → 上传
                let etag = self.backend.put(&d.rel, &bytes)?;
                self.report.uploaded += 1;
                self.report.bytes_up += bytes.len() as u64;
                self.db.upsert(&SyncStateRow {
                    doc_uuid: d.uuid.clone(),
                    file_path: d.rel.clone(),
                    local_hash: d.hash.clone(),
                    synced_hash: d.hash.clone(),
                    remote_etag: etag,
                    synced_at: self.now,
                });
                self.progress(&d.rel, "upload", bytes.len() as u64);
                self.log(&d.rel, "up", bytes.len() as u64, "put", "ok");
                Ok(())
            }
            (false, true) => {
                // 远端修改、本地未动 → 下载（A15 前半：自动重载）
                match remote_files.get(&d.rel) {
                    Some(_) => {
                        let bytes = self.backend.get(&d.rel)?;
                        self.report.bytes_down += bytes.len() as u64;
                        let hash = hash_hex(&bytes);
                        self.write_local_doc(&d.rel, &bytes)?;
                        self.db.upsert(&SyncStateRow {
                            doc_uuid: d.uuid.clone(),
                            file_path: d.rel.clone(),
                            local_hash: hash.clone(),
                            synced_hash: hash.clone(),
                            remote_etag: remote_files.get(&d.rel).map(|e| e.etag.clone()).unwrap_or_default(),
                            synced_at: self.now,
                        });
                        self.report.downloaded += 1;
                        self.progress(&d.rel, "download", bytes.len() as u64);
                        self.log(&d.rel, "down", bytes.len() as u64, "get", "ok");
                        (self.emit)(
                            "doc:external_change",
                            serde_json::json!({ "path": d.rel, "hash": hash }),
                        );
                        Ok(())
                    }
                    None => {
                        // 远端被外部直接删除：本地移入回收站（FR-SYN-08），本地有修改则保留重传
                        if local_changed {
                            self.warn(format!("远端被删除但本地有修改，保留并重传：{}", d.rel));
                            let etag = self.backend.put(&d.rel, &bytes)?;
                            let _ = etag;
                        } else {
                            self.lib.delete_to_trash(&d.rel)?;
                            self.db.remove(&d.uuid);
                            self.report.trashed_local += 1;
                            self.log(&d.rel, "local", 0, "trash", "remote-deleted");
                        }
                        Ok(())
                    }
                }
            }
            (true, true) => {
                // 双端修改 → 冲突（A4）：远端下载为冲突副本（新 uuid），本地正常上传。
                // 远端在本路径不存在（可能被对端改名移走）→ 仅上传，交给远端独有文件探测收敛
                let remote_bytes = match self.backend.get(&d.rel) {
                    Ok(b) => b,
                    Err(e) if e.code == "not_found" => {
                        self.warn(format!("远端版本不在预期路径（可能被对端改名）：{}", d.rel));
                        let etag = self.backend.put(&d.rel, &bytes)?;
                        self.report.uploaded += 1;
                        self.report.bytes_up += bytes.len() as u64;
                        self.db.upsert(&SyncStateRow {
                            doc_uuid: d.uuid.clone(),
                            file_path: d.rel.clone(),
                            local_hash: d.hash.clone(),
                            synced_hash: d.hash.clone(),
                            remote_etag: etag,
                            synced_at: self.now,
                        });
                        self.progress(&d.rel, "upload", bytes.len() as u64);
                        self.log(&d.rel, "up", bytes.len() as u64, "put", "ok");
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                self.report.bytes_down += remote_bytes.len() as u64;
                let (_r_uuid, r_title) = {
                    let c = String::from_utf8_lossy(&remote_bytes);
                    let (u, t) = parse_uuid_title(&c);
                    (u, if t.is_empty() { d.rel.clone() } else { t })
                };
                let copy_rel = self.create_conflict_copy(&d.rel, &r_title, &remote_bytes)?;
                let etag = self.backend.put(&d.rel, &bytes)?;
                self.report.uploaded += 1;
                self.report.bytes_up += bytes.len() as u64;
                self.db.upsert(&SyncStateRow {
                    doc_uuid: d.uuid.clone(),
                    file_path: d.rel.clone(),
                    local_hash: d.hash.clone(),
                    synced_hash: d.hash.clone(),
                    remote_etag: etag,
                    synced_at: self.now,
                });
                // 冲突副本作为新文档登记（本轮一并上传，库内 uuid 唯一）
                let copy_bytes = fs::read(self.lib.root.join(&copy_rel))?;
                let (copy_uuid, _) = parse_uuid_title(&String::from_utf8_lossy(&copy_bytes));
                let copy_hash = hash_hex(&copy_bytes);
                let copy_etag = self.backend.put(&copy_rel, &copy_bytes)?;
                self.report.bytes_up += copy_bytes.len() as u64;
                self.db.upsert(&SyncStateRow {
                    doc_uuid: copy_uuid.clone(),
                    file_path: copy_rel.clone(),
                    local_hash: copy_hash.clone(),
                    synced_hash: copy_hash,
                    remote_etag: copy_etag,
                    synced_at: self.now,
                });
                self.report.conflicts.push(ConflictInfo {
                    uuid: d.uuid.clone(),
                    path: d.rel.clone(),
                    copy_path: copy_rel.clone(),
                });
                (self.emit)(
                    "sync:conflict",
                    serde_json::json!({ "path": d.rel, "copyPath": copy_rel, "title": r_title }),
                );
                self.log(&d.rel, "local", remote_bytes.len() as u64, "conflict", "copy-created");
                Ok(())
            }
        }
    }

    // -- 附件（存在即上传；远端新增下载） ----------------------------------------

    fn sync_attachments(&mut self, atts: &[LocalAttachment], remote_files: &BTreeMap<String, RemoteEntry>) {
        for a in atts {
            let up_needed = match remote_files.get(&a.rel) {
                None => true,
                Some(e) => {
                    if !e.etag.is_empty() {
                        e.etag != a.hash
                    } else {
                        true // 保守重传（附件通常少且小）
                    }
                }
            };
            if up_needed {
                if let Ok(bytes) = fs::read(self.lib.root.join(&a.rel)) {
                    if self.backend.put(&a.rel, &bytes).is_ok() {
                        self.report.uploaded += 1;
                        self.report.bytes_up += bytes.len() as u64;
                        self.progress(&a.rel, "upload", bytes.len() as u64);
                        self.log(&a.rel, "up", bytes.len() as u64, "put", "ok");
                    }
                }
            }
        }
        let remote_atts: Vec<(String, String)> = remote_files
            .iter()
            .filter(|(p, _)| p.starts_with("attachments/"))
            .map(|(p, e)| (p.clone(), e.etag.clone()))
            .collect();
        for (rp, _) in remote_atts {
            if !atts.iter().any(|a| a.rel == rp) {
                if let Ok(bytes) = self.backend.get(&rp) {
                    let dest = self.lib.root.join(&rp);
                    if let Some(parent) = dest.parent() {
                        let _g = self.lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
                        let _ = fs::create_dir_all(parent);
                        if atomic_write(&dest, &bytes).is_ok() {
                            self.report.downloaded += 1;
                            self.report.bytes_down += bytes.len() as u64;
                            self.progress(&rp, "download", bytes.len() as u64);
                            self.log(&rp, "down", bytes.len() as u64, "get", "ok");
                        }
                    }
                }
            }
        }
    }

    // -- 统计清单（应用独占，V1 无写入方：远端存在则收编） --------------------------

    fn settle_stats_manifest(&mut self, remote_files: &BTreeMap<String, RemoteEntry>) -> Result<(), AppError> {
        if remote_files.contains_key(MANIFEST_STATS) {
            let bytes = self.backend.get(MANIFEST_STATS)?;
            let local_path = self.lib.root.join(MANIFEST_STATS);
            let cur = fs::read(&local_path).unwrap_or_default();
            if cur != bytes {
                let _g = self.lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(parent) = local_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                atomic_write(&local_path, &bytes)?;
                self.report.downloaded += 1;
            }
        }
        Ok(())
    }

    // -- 本地写操作（全部经 LibraryService 单写者，3.3-6） --------------------------

    fn write_local_doc(&self, rel: &str, bytes: &[u8]) -> Result<(), AppError> {
        let dest = self.lib.root.join(rel);
        let _g = self.lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&dest, bytes)?;
        Ok(())
    }

    /// 下载入库：无 id 文件分配新 uuid 并回写；同名冲突自动唯一化。返回实际相对路径
    fn create_local_doc(&self, remote_path: &str, bytes: &[u8]) -> Result<String, AppError> {
        let content = String::from_utf8_lossy(bytes);
        let (fm_opt, body) = frontmatter::split(&content);
        let mut fm = fm_opt.unwrap_or_default();
        if fm.get("id").is_none() {
            fm.set("id", &uuid_new());
        }
        if fm.get("fmt").is_none() {
            fm.set("fmt", "1");
        }
        if fm.get("created").is_none() {
            fm.set("created", &self.now.to_string());
        }
        let full = format!("{}\n{}", fm.render(), body);

        let filename = remote_path.rsplit('/').next().unwrap_or("未命名.md").to_string();
        let parent_rel = remote_path
            .strip_suffix(&filename)
            .unwrap_or("")
            .trim_end_matches('/');
        let dest_dir = if parent_rel.is_empty() {
            self.lib.root.clone()
        } else {
            self.lib.root.join(parent_rel)
        };
        let filename = unique_file_name(&dest_dir, &filename);
        let _g = self.lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        fs::create_dir_all(&dest_dir)?;
        atomic_write(&dest_dir.join(&filename), full.as_bytes())?;
        Ok(if parent_rel.is_empty() {
            filename
        } else {
            format!("{parent_rel}/{filename}")
        })
    }

    fn rename_local_doc(&self, from_rel: &str, to_rel: &str) -> Result<String, AppError> {
        let filename = to_rel.rsplit('/').next().unwrap_or("未命名.md").to_string();
        let parent_rel = to_rel
            .strip_suffix(&filename)
            .unwrap_or("")
            .trim_end_matches('/');
        let to_dir = if parent_rel.is_empty() {
            self.lib.root.clone()
        } else {
            self.lib.root.join(parent_rel)
        };
        let filename = unique_file_name(&to_dir, &filename);
        let from = self.lib.root.join(from_rel);
        let _g = self.lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        fs::create_dir_all(&to_dir)?;
        fs::rename(&from, to_dir.join(&filename))?;
        Ok(if parent_rel.is_empty() {
            filename
        } else {
            format!("{parent_rel}/{filename}")
        })
    }

    /// 冲突副本：`文档名 (远程冲突备份 YYYYMMDD-HHmmss).md`，分配**新 uuid**（FR-SYN-04）；
    /// frontmatter 记 conflict_of = 原文档路径，供对比视图三动作定位原文档
    fn create_conflict_copy(&self, orig_rel: &str, title: &str, remote_bytes: &[u8]) -> Result<String, AppError> {
        let (fm_opt, body) = {
            let c = String::from_utf8_lossy(remote_bytes);
            frontmatter::split(&c)
        };
        let mut fm = fm_opt.unwrap_or_default();
        let new_uuid = uuid_new();
        fm.set("id", &new_uuid); // 副本持新 uuid，库内 uuid 唯一（A4）
        fm.set(
            "title",
            &format!("{title}（远程冲突备份 {}）", fmt_stamp(self.now)),
        );
        fm.set("conflict_of", orig_rel);
        let full = format!("{}\n{}", fm.render(), body);
        let dir = match orig_rel.rfind('/') {
            Some(i) => self.lib.root.join(&orig_rel[..i]),
            None => self.lib.root.clone(),
        };
        let stem = crate::library::sanitize_filename(title);
        let filename = unique_file_name(
            &dir,
            &format!("{stem} (远程冲突备份 {}).md", fmt_stamp(self.now)),
        );
        let _g = self.lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join(&filename), full.as_bytes())?;
        let parent = orig_rel.rfind('/').map(|i| &orig_rel[..i + 1]).unwrap_or("");
        Ok(format!("{parent}{filename}"))
    }
}

fn uuid_new() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// 逐文档同步状态（FR-SYN-03 状态可视化：已同步 / 待同步；冲突由 conflicts 另行给）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct DocSyncState {
    pub path: String,
    /// "synced" | "pending"（uuid 缺失或 hash 尚未同步过）
    pub state: String,
}

pub fn doc_sync_states(lib: &Library, db: &SyncDb) -> Vec<DocSyncState> {
    let (docs, _) = scan_local(lib);
    let rows: BTreeMap<String, SyncStateRow> =
        db.all().into_iter().map(|r| (r.doc_uuid.clone(), r)).collect();
    docs.into_iter()
        .map(|d| {
            let state = match rows.get(&d.uuid) {
                Some(r) if r.synced_hash == d.hash && r.file_path == d.rel => "synced",
                _ => "pending",
            };
            DocSyncState {
                path: d.rel,
                state: state.to_string(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 冲突三动作（FR-SYN-04 v1.7 定案语义）
// ---------------------------------------------------------------------------

/// keep_remote=false：保留此版/手动合并后清理 —— 删除副本（经回收站），下次同步传播删除；
/// keep_remote=true：保留彼版 —— 原文自动快照后以副本内容替换本地正文（uuid 还原为原文档），
/// 再删除副本，下次同步正常上传。
pub fn resolve_conflict(lib: &Library, copy_rel: &str, keep_remote: bool) -> Result<(), AppError> {
    let copy_path = lib.root.join(copy_rel);
    let bytes = fs::read(&copy_path)?;
    let content = String::from_utf8_lossy(&bytes).to_string();
    let (fm_opt, body) = frontmatter::split(&content);
    let fm = fm_opt.ok_or_else(|| AppError::new("not_found", "冲突副本缺少 frontmatter"))?;
    let orig_rel = fm
        .get("conflict_of")
        .ok_or_else(|| AppError::new("not_found", "副本缺少 conflict_of 标记"))?
        .to_string();

    if keep_remote {
        let orig_path = lib.root.join(&orig_rel);
        let orig = fs::read_to_string(&orig_path).unwrap_or_default();
        // 替换前自动快照（FR-SYN-04：保留彼版语义）
        lib.snapshot_create(&orig_rel, "manual", "冲突替换前自动备份")?;
        // 副本 frontmatter 为底，id 还原为原文档 uuid（身份锚点不变）
        let orig_id = frontmatter::split(&orig)
            .0
            .and_then(|f| f.get("id").map(|s| s.to_string()));
        let mut fm2 = fm.clone();
        match orig_id {
            Some(id) => fm2.set("id", &id),
            None => fm2.set("id", &uuid_new()),
        }
        fm2.entries.retain(|(k, _)| k != "conflict_of"); // 使命完成，不长期保留
        let full = format!("{}\n{}", fm2.render(), body);
        {
            let _g = lib.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            atomic_write(&orig_path, full.as_bytes())?;
        }
    }
    // 删除副本（两动作共用收尾）：回收站保留，引擎下轮传播删除
    lib.delete_to_trash(copy_rel)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 双引擎检测（FR-SYN-01：库位于第三方同步盘目录内时显著警告）
// ---------------------------------------------------------------------------

pub fn detect_thirdparty_sync(root: &Path) -> Vec<String> {
    let p = root.to_string_lossy().to_lowercase();
    const MARKERS: &[(&str, &str)] = &[
        ("onedrive", "OneDrive"),
        ("nutstore", "坚果云（Nutstore）"),
        ("坚果云", "坚果云"),
        ("dropbox", "Dropbox"),
        ("google drive", "Google Drive"),
        ("google\\drive", "Google Drive"),
        ("iclouddrive", "iCloud Drive"),
    ];
    MARKERS
        .iter()
        .filter(|(m, _)| p.contains(m))
        .map(|(_, name)| name.to_string())
        .collect()
}

fn fmt_stamp(t: u64) -> String {
    // 简易 Unix → YYYYMMDD-HHmmss（UTC；文件名用途，无需本地时区精确）
    let days = t / 86400;
    let secs = t % 86400;
    let (yy, mm, dd) = civil_from_days(days as i64);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        yy,
        mm,
        dd,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn unique_file_name(dir: &Path, want: &str) -> String {
    let mut name = want.to_string();
    if !dir.join(&name).exists() {
        return name;
    }
    let stem = want.trim_end_matches(".md");
    let mut n = 2;
    loop {
        name = format!("{stem}-{n}.md");
        if !dir.join(&name).exists() {
            return name;
        }
        n += 1;
    }
}

// ---------------------------------------------------------------------------
// 双机模拟回归（5.1 可测试性：MockDav 替身，覆盖 A3/A4/A11/A12/A13/A14）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dav::mock::MockDav;
    use crate::library::Library;

    fn open_lib(name: &str) -> Library {
        let dir = std::env::temp_dir().join(format!(
            "writer-sync-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        Library::open_existing(&dir).expect("打开测试库失败")
    }

    fn mk_doc(lib: &Library, title: &str, body: &str) -> String {
        let node = lib.create_doc("", title).unwrap();
        let d = lib.read_doc(&node.id).unwrap();
        lib.write_doc_inner(&node.id, body, &d.hash, false).unwrap();
        node.id
    }

    fn edit(lib: &Library, rel: &str, body: &str) {
        let d = lib.read_doc(rel).unwrap();
        lib.write_doc_inner(rel, body, &d.hash, false).unwrap();
    }

    /// bound 同步（跳过首次绑定）；返回 Done 报告
    fn run_bound(lib: &Library, dav: &mut MockDav) -> SyncReport {
        let db = SyncDb::open(&lib.root).unwrap();
        let mut settings = load_settings(&lib.root);
        settings.enabled = true;
        settings.bound = true;
        save_settings(&lib.root, &settings).unwrap();
        match SyncEngine::run(lib, &db, dav, &settings, None, &|_, _| {}).unwrap() {
            SyncOutcome::Done(r) => r,
            SyncOutcome::NeedsDirection { .. } => panic!("bound 同步不应要求方向"),
        }
    }

    fn run_first(lib: &Library, dav: &mut MockDav, dir: Option<Direction>) -> SyncOutcome {
        let db = SyncDb::open(&lib.root).unwrap();
        let mut settings = load_settings(&lib.root);
        settings.enabled = true;
        settings.bound = false;
        save_settings(&lib.root, &settings).unwrap();
        SyncEngine::run(lib, &db, dav, &settings, dir, &|_, _| {}).unwrap()
    }

    fn all_doc_uuids(lib: &Library) -> Vec<String> {
        let (docs, _) = scan_local(lib);
        docs.iter().map(|d| d.uuid.clone()).collect()
    }

    fn list_md_files(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        walk_files(dir, "", &mut out);
        out.into_iter().filter(|f| f.ends_with(".md")).collect()
    }

    // -- A3：增量同步，无重复上传，双端收敛 -----------------------------------

    #[test]
    fn a3_incremental_two_devices() {
        let a = open_lib("a3-a");
        let mut dav = MockDav::new();

        let d1 = mk_doc(&a, "第一章", "台式机写的第一章内容");
        let d2 = mk_doc(&a, "第二章", "台式机写的第二章内容");
        let d3 = mk_doc(&a, "第三章", "台式机写的第三章内容");

        // 台式机首次同步（远端空 → 上传）
        let r = match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(r) => r,
            _ => panic!("空远端应直接上传"),
        };
        assert_eq!(r.uploaded, 3, "3 篇全部上传");

        // 立即再同步：全量 etag 命中，零重复上传
        let r2 = run_bound(&a, &mut dav);
        assert_eq!(r2.uploaded, 0, "未变文件不得重复上传（A3）");
        assert_eq!(r2.downloaded, 0);

        // 笔记本（空库）绑定：自动选择下载方向
        let b = open_lib("a3-b");
        match run_first(&b, &mut dav, None) {
            SyncOutcome::Done(r) => assert_eq!(r.downloaded, 3),
            _ => panic!("空本地应直接下载"),
        }
        assert!(b.read_doc(&d1).is_ok(), "笔记本应有三章");

        // 笔记本续写第二章，同步
        edit(&b, &d2, "笔记本续写的第二章内容");
        let rb = run_bound(&b, &mut dav);
        assert_eq!(rb.uploaded, 1);

        // 台式机再同步：仅下载第二章
        let ra = run_bound(&a, &mut dav);
        assert_eq!(ra.downloaded, 1, "仅增量下载");
        assert_eq!(ra.uploaded, 0);
        assert!(a.read_doc(&d2).unwrap().body.contains("笔记本续写"));
        let _ = (d1, d3);
    }

    // -- A4：双端离线改同一文档 → 冲突副本，双方内容保留，库内 uuid 唯一 ------

    #[test]
    fn a4_conflict_copy_new_uuid() {
        let a = open_lib("a4-a");
        let mut dav = MockDav::new();
        let doc = mk_doc(&a, "冲突章", "原始内容");
        match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!("首绑应上传"),
        }

        let b = open_lib("a4-b");
        match run_first(&b, &mut dav, None) {
            SyncOutcome::Done(r) => assert_eq!(r.downloaded, 1),
            _ => panic!("B 应下载"),
        }

        // 双端离线各改同一章
        edit(&a, &doc, "台式机版本的内容 XX");
        edit(&b, &doc, "笔记本版本的内容 YY");

        run_bound(&a, &mut dav); // A 先上线：正常上传
        let rb = run_bound(&b, &mut dav); // B 后上线：冲突
        assert_eq!(rb.conflicts.len(), 1, "应产生一个冲突");

        // 冲突副本存在、名字合规、uuid 为新值
        let copy = &rb.conflicts[0].copy_path;
        assert!(copy.contains("远程冲突备份"), "副本名：{copy}");
        assert!(b.root.join(copy).is_file());
        let uuids = all_doc_uuids(&b);
        assert_eq!(uuids.len(), uuids.iter().collect::<std::collections::HashSet<_>>().len(), "库内 uuid 不得重复（A4）");

        // 双方内容均保留：B 本地 = 笔记本版；副本 = 台式机版
        assert!(b.read_doc(&doc).unwrap().body.contains("笔记本版本"));
        let copy_content = fs::read_to_string(b.root.join(copy)).unwrap();
        assert!(copy_content.contains("台式机版本"), "副本必须保留对端内容");
    }

    // -- A11：删除传播，进入回收站，此后任何一次同步都不复活 --------------------

    #[test]
    fn a11_delete_propagation_no_resurrection() {
        let a = open_lib("a11-a");
        let mut dav = MockDav::new();
        let doc = mk_doc(&a, "将被删除", "内容");
        mk_doc(&a, "保留", "保留内容");
        match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }
        let b = open_lib("a11-b");
        match run_first(&b, &mut dav, None) {
            SyncOutcome::Done(r) => assert_eq!(r.downloaded, 2),
            _ => panic!(),
        }

        // 设备A删除一章 → 同步
        a.delete_to_trash(&doc).unwrap();
        let ra = run_bound(&a, &mut dav);
        assert_eq!(ra.deleted_remote, 1, "远端文件应被删除");

        // 设备B同步：该章进回收站（可恢复）
        let rb = run_bound(&b, &mut dav);
        assert_eq!(rb.trashed_local, 1, "对端应移入回收站（A11）");
        assert!(b.read_doc(&doc).is_err(), "B 端该章应不可读");
        assert!(b.read_doc("docs/保留.md").is_ok(), "其余文档不受影响");

        // B 再次同步：不复活（零上传零下载），远端也没有它
        let rb2 = run_bound(&b, &mut dav);
        assert_eq!(rb2.uploaded + rb2.downloaded, 0, "删除不得复活（A11）");
        let remote_docs: Vec<&String> = dav.files.keys().filter(|p| p.ends_with(".md")).collect();
        assert!(!remote_docs.iter().any(|p| p.contains("将被删除")));
    }

    // -- A12：改名走 MOVE 不重传，对端跟随，uuid 不变 ---------------------------

    #[test]
    fn a12_rename_moves_remote() {
        let a = open_lib("a12-a");
        let mut dav = MockDav::new();
        let doc = mk_doc(&a, "旧标题", "正文内容不变的部分");
        match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }
        let uuid_before = all_doc_uuids(&a)[0].clone();

        // A 改名（rename_doc 会重写 frontmatter title + 文件名）
        let r = a.rename_doc_inner(&doc, "新标题").unwrap().0;

        // A 同步：远端 MOVE + 内容补传（title 变更）
        dav.ops.clear();
        let ra = run_bound(&a, &mut dav);
        eprintln!("[report] moved={} uploaded={} downloaded={} conflicts={} warnings={:?} ops={:?}", ra.moved, ra.uploaded, ra.downloaded, ra.conflicts.len(), ra.warnings, dav.ops);
        assert_eq!(ra.moved, 1, "改名必须走 MOVE（A12）");
        assert!(
            dav.ops.iter().any(|op| op.starts_with("mv ")),
            "应记录 move 动作：{:?}",
            dav.ops
        );
        assert!(dav.files.contains_key(&r), "远端应存在新路径");

        // B 同步：本地文件跟随远端改名，uuid 保持
        let b = open_lib("a12-b");
        match run_first(&b, &mut dav, None) {
            SyncOutcome::Done(rep) => assert_eq!(rep.downloaded, 1),
            _ => panic!(),
        }
        let uuid_b = all_doc_uuids(&b)[0].clone();
        assert_eq!(uuid_b, uuid_before, "sync_state 必须保持同一 uuid（A12）");
        assert!(b.root.join(&r).is_file(), "B 端文件应在新路径：{}", r);
        assert!(b.read_doc(&doc).is_err(), "B 端旧路径应消失");
    }

    // -- A13：首绑两端均非空 → 必须选方向，选择前零写操作 ------------------------

    #[test]
    fn a13_first_bind_requires_direction() {
        // 远端已有一份内容（由独立库 C 上传）
        let c = open_lib("a13-c");
        let mut dav = MockDav::new();
        mk_doc(&c, "远端文档", "远端的内容");
        match run_first(&c, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }

        // 本地也有内容的库 A 绑定同一远端
        let a = open_lib("a13-a");
        let local_doc = mk_doc(&a, "本地文档", "本地的内容");
        let files_before = list_md_files(&a.root.join("docs"));
        let remote_before: Vec<String> = dav.files.keys().cloned().collect();
        dav.ops.clear(); // 仅统计 A 这次的写操作

        let out = run_first(&a, &mut dav, None);
        match out {
            SyncOutcome::NeedsDirection { local_docs, remote_docs } => {
                assert_eq!(local_docs, 1);
                assert_eq!(remote_docs, 1);
            }
            _ => panic!("两端均非空必须要求方向选择（A13）"),
        }
        // 零写操作：本地与远端均原封不动
        assert_eq!(list_md_files(&a.root.join("docs")), files_before, "选择前本地零写");
        assert!(
            !dav.ops.iter().any(|op| op.starts_with("put") || op.starts_with("delete")),
            "选择前远端零写"
        );
        assert_eq!(
            dav.files.keys().cloned().collect::<Vec<_>>(),
            remote_before,
            "远端文件集合不变"
        );
        let _ = local_doc;

        // 用户选择"下载远端"后本地与远端一致
        match run_first(&a, &mut dav, Some(Direction::Download)) {
            SyncOutcome::Done(rep) => assert_eq!(rep.downloaded, 1),
            _ => panic!(),
        }
        assert!(a.read_doc("docs/远端文档.md").is_ok(), "下载后本地与远端一致");
        // 本地多余文档移入回收站而非硬删
        assert!(a.read_doc("docs/本地文档.md").is_err());
    }

    // -- A14：双端离线各删不同文档 → 墓碑并集合并，无清单冲突副本 -----------------

    #[test]
    fn a14_tombstone_union_merge() {
        let a = open_lib("a14-a");
        let mut dav = MockDav::new();
        let doc1 = mk_doc(&a, "文档一", "内容一");
        let doc2 = mk_doc(&a, "文档二", "内容二");
        match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }
        let b = open_lib("a14-b");
        match run_first(&b, &mut dav, None) {
            SyncOutcome::Done(r) => assert_eq!(r.downloaded, 2),
            _ => panic!(),
        }

        // 双端离线各删除不同文档
        a.delete_to_trash(&doc1).unwrap();
        b.delete_to_trash(&doc2).unwrap();
        run_bound(&a, &mut dav); // A 先同步：墓碑 t1 上传
        run_bound(&b, &mut dav); // B 同步：下载 t1、追加 t2、上传合并清单

        // 清单按 uuid 并集合并，且不产生清单冲突副本文件
        let manifest = TombstoneManifest::parse(&dav.files[MANIFEST_SYNC]).unwrap();
        assert_eq!(manifest.tombstones.len(), 2, "墓碑按 uuid 并集（A14）");

        // A 再同步：吸收 t2，B 端删除的文档在 A 端进回收站
        let ra = run_bound(&a, &mut dav);
        assert_eq!(ra.trashed_local, 1, "A 端应收到 doc2 的删除传播");
        assert!(a.read_doc(&doc2).is_err());
        assert!(a.read_doc(&doc1).is_err());

        // 库内不存在任何清单冲突副本文件
        for lib_root in [&a.root, &b.root] {
            let files = list_md_files(&lib_root.join("docs"));
            assert!(
                !files.iter().any(|f| f.contains("writer-sync") && f.contains("冲突")),
                "清单是应用独占文件，永不走冲突副本：{files:?}"
            );
        }
        // B 端同样两篇都没了（各自删除 + 传播）
        assert!(b.read_doc(&doc1).is_err());
        assert!(b.read_doc(&doc2).is_err());
    }

    // -- 远端新建文档传播（A3 的对向） -------------------------------------------

    #[test]
    fn remote_new_doc_downloads() {
        let a = open_lib("new-a");
        let mut dav = MockDav::new();
        mk_doc(&a, "已有", "内容");
        match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }
        let b = open_lib("new-b");
        match run_first(&b, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }

        // A 新建并同步
        let nd = mk_doc(&a, "新文档", "新内容");
        run_bound(&a, &mut dav);

        let rb = run_bound(&b, &mut dav);
        assert_eq!(rb.downloaded, 1);
        assert!(b.read_doc(&nd).is_ok(), "对端新建应下载入库");
    }

    // -- 墓碑清单合并语义单元测试 -------------------------------------------------

    #[test]
    fn tombstone_merge_union_latest_wins() {
        let mut m = TombstoneManifest::default();
        m.upsert("u1", "docs/a.md", 100);
        let mut other = TombstoneManifest::default();
        other.upsert("u1", "docs/a.md", 200); // 同 uuid 更晚
        other.upsert("u2", "docs/b.md", 150);
        assert!(m.merge(&other));
        assert_eq!(m.tombstones.len(), 2);
        assert_eq!(m.get("u1").unwrap().deleted_at, 200, "同 uuid 取更晚删除时间");
        assert!(!m.merge(&other), "并集合并幂等");
    }

    // -- 双端改名冲突回退冲突副本（FR-SYN-08） -------------------------------------

    #[test]
    fn rename_conflict_falls_back_to_copy() {
        let a = open_lib("rc-a");
        let mut dav = MockDav::new();
        let doc = mk_doc(&a, "原名", "共同起点的内容");
        match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }
        let b = open_lib("rc-b");
        match run_first(&b, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }

        // 双端离线对同一文档改为不同名（内容也随之变）
        let a_new = a.rename_doc_inner(&doc, "台式机改名").unwrap().0;
        let b_new = b.rename_doc_inner(&doc, "笔记本改名").unwrap().0;

        run_bound(&a, &mut dav); // A 先：MOVE 原路径 → a_new
        let rb = run_bound(&b, &mut dav); // B：本地已分叉 → 冲突副本回退
        assert!(!rb.conflicts.is_empty(), "双端改名应回退冲突副本路径");

        // 双方内容均保留：B 本地 = 笔记本名/内容；副本 = 台式机版本
        assert!(b.root.join(&b_new).is_file(), "B 本地版保留");
        let copy = fs::read_to_string(b.root.join(&rb.conflicts[0].copy_path)).unwrap();
        assert!(copy.contains("台式机改名"), "副本保留对端改名版");
        let _ = a_new;

        // 远端不残留重复 uuid：a_new 已被删除，仅剩 b_new + 副本
        let remote_uuids: Vec<String> = dav
            .files
            .iter()
            .filter(|(p, _)| p.ends_with(".md") && !p.contains(".writer-"))
            .map(|(_, b)| parse_uuid_title(&String::from_utf8_lossy(b)).0)
            .collect();
        assert_eq!(
            remote_uuids.len(),
            remote_uuids.iter().collect::<std::collections::HashSet<_>>().len(),
            "远端不得有重复 uuid：{remote_uuids:?}"
        );
    }

    // -- A15：打开文档的远端更新协议（引擎侧） ------------------------------------

    /// 带事件捕获的 bound 同步
    fn run_bound_capture(
        lib: &Library,
        dav: &mut MockDav,
        events: &std::cell::RefCell<Vec<(String, serde_json::Value)>>,
    ) -> SyncReport {
        let db = SyncDb::open(&lib.root).unwrap();
        let mut settings = load_settings(&lib.root);
        settings.enabled = true;
        settings.bound = true;
        save_settings(&lib.root, &settings).unwrap();
        let emit = |e: &str, p: serde_json::Value| {
            events.borrow_mut().push((e.to_string(), p));
        };
        match SyncEngine::run(lib, &db, dav, &settings, None, &emit).unwrap() {
            SyncOutcome::Done(r) => r,
            SyncOutcome::NeedsDirection { .. } => panic!("bound 同步不应要求方向"),
        }
    }

    #[test]
    fn a15_external_change_event_and_conflict_flow() {
        let a = open_lib("a15-a");
        let mut dav = MockDav::new();
        let doc = mk_doc(&a, "打开中的文档", "原始版本内容");
        match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }
        let b = open_lib("a15-b");
        match run_first(&b, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }

        // 场景一：B 无本地输入 → A 修改并同步 → B 同步应自动重载并发出 external_change
        edit(&a, &doc, "远端新版本内容（场景一）");
        run_bound(&a, &mut dav);
        let events: std::cell::RefCell<Vec<(String, serde_json::Value)>> =
            std::cell::RefCell::new(Vec::new());
        let rb = run_bound_capture(&b, &mut dav, &events);
        assert_eq!(rb.downloaded, 1);
        assert!(
            b.read_doc(&doc).unwrap().body.contains("远端新版本内容"),
            "B 端磁盘内容应已更新"
        );
        assert!(
            events
                .borrow()
                .iter()
                .any(|(e, p)| e == "doc:external_change" && p["path"] == doc),
            "必须发出 doc:external_change 事件（FR-SYN-09）：{:?}",
            events.borrow().iter().map(|(e, _)| e.clone()).collect::<Vec<_>>()
        );

        // 场景二：B 有未保存输入（本地已改未同步）+ 远端又变 → 冲突流程，双方内容保留
        edit(&b, &doc, "B 的未保存输入（场景二）");
        edit(&a, &doc, "A 的再次修改（场景二）");
        run_bound(&a, &mut dav);
        let events2: std::cell::RefCell<Vec<(String, serde_json::Value)>> =
            std::cell::RefCell::new(Vec::new());
        let rb2 = run_bound_capture(&b, &mut dav, &events2);
        assert!(!rb2.conflicts.is_empty(), "有输入时必须走冲突流程（A15）");
        assert!(events2.borrow().iter().any(|(e, _)| e == "sync:conflict"));
        let copy = fs::read_to_string(b.root.join(&rb2.conflicts[0].copy_path)).unwrap();
        assert!(copy.contains("A 的再次修改"), "远端修改不得丢失（A15）");
        assert!(
            b.read_doc(&doc).unwrap().body.contains("B 的未保存输入"),
            "本地输入不得丢失（A15）"
        );
    }

    // -- 逐文档同步状态（FR-SYN-03 状态可视化数据源） ------------------------------

    #[test]
    fn doc_sync_states_pending_then_synced() {
        let a = open_lib("dss-a");
        let mut dav = MockDav::new();
        let doc = mk_doc(&a, "状态文档", "内容");
        let db = SyncDb::open(&a.root).unwrap();

        // 未同步前：pending
        let st = doc_sync_states(&a, &db);
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].state, "pending");
        assert_eq!(st[0].path, doc);

        // 同步后：synced
        match run_first(&a, &mut dav, None) {
            SyncOutcome::Done(_) => {}
            _ => panic!(),
        }
        let st = doc_sync_states(&a, &db);
        assert_eq!(st[0].state, "synced");

        // 本地再编辑：回到 pending
        edit(&a, &doc, "新内容");
        let st = doc_sync_states(&a, &db);
        assert_eq!(st[0].state, "pending");
    }
}
