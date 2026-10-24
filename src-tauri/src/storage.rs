//! 存储空间面板（FR-PKG-05）+ 备份恢复向导（FR-SAV-05），V1.1。
//! 存储面板：快照/滚动备份/回收站/附件四类占用统计与一键清理，保留策略调整。
//! 恢复向导：从库内备份或用户选择的 zip 恢复——恢复前自动再备份当前状态
//!（防"恢复本身成为事故"）；支持整库恢复或按篇勾选（重名生成副本）。
//! AC：清空 docs/ 后从昨日备份完整恢复，uuid 与顺序不变。

use std::io::Read;
use std::path::Path;

use crate::library::{atomic_write, AppError};

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

fn count_entries(dir: &Path, filter: impl Fn(&str) -> bool) -> (usize, u64) {
    let mut n = 0usize;
    let mut bytes = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !filter(&name) {
                continue;
            }
            if let Ok(m) = e.metadata() {
                n += 1;
                bytes += m.len();
            }
        }
    }
    (n, bytes)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageInfo {
    pub snapshots_count: usize,
    pub snapshots_bytes: u64,
    pub backups_count: usize,
    pub backups_bytes: u64,
    pub trash_count: usize,
    pub trash_bytes: u64,
    pub attachments_count: usize,
    pub attachments_bytes: u64,
    pub docs_bytes: u64,
    pub total_bytes: u64,
    /// 保留策略（可调）：快照保留天数、备份保留份数
    pub snapshot_keep_days: u64,
    pub backup_keep: usize,
}

pub fn info(root: &Path) -> StorageInfo {
    let (sn, sb) = count_entries(&root.join(".writer").join("snapshots"), |_| true);
    let (bn, bb) = count_entries(&root.join(".writer").join("backups"), |n| {
        n.starts_with("backup-") && n.ends_with(".zip")
    });
    let (tn, tb) = count_entries(&root.join(".writer").join("trash"), |n| n != "index.json");
    let (an, ab) = count_entries(&root.join("attachments"), |_| true);
    let db = dir_size(&root.join("docs"));
    StorageInfo {
        snapshots_count: sn,
        snapshots_bytes: sb,
        backups_count: bn,
        backups_bytes: bb,
        trash_count: tn,
        trash_bytes: tb,
        attachments_count: an,
        attachments_bytes: ab,
        docs_bytes: db,
        total_bytes: sb + bb + tb + ab + db,
        snapshot_keep_days: snapshot_keep_days(root),
        backup_keep: backup_keep(root),
    }
}

fn kv_read_u64(root: &Path, key: &str, default: u64) -> u64 {
    crate::search::open(root)
        .ok()
        .and_then(|c| {
            c.query_row("SELECT v FROM kv WHERE k = ?1", [key], |r| r.get::<_, String>(0))
                .ok()
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn kv_write(root: &Path, key: &str, val: &str) -> Result<(), AppError> {
    let c = crate::search::open(root)?;
    c.execute(
        "INSERT OR REPLACE INTO kv(k, v) VALUES (?1, ?2)",
        [key, val],
    )
    .map_err(|e| AppError::new("storage", e.to_string()))?;
    Ok(())
}

pub fn snapshot_keep_days(root: &Path) -> u64 {
    kv_read_u64(root, "snapshot_keep_days", 90)
}

pub fn backup_keep(root: &Path) -> usize {
    kv_read_u64(root, "backup_keep", 7) as usize
}

pub fn set_policy(root: &Path, snapshot_days: u64, backup_keep_n: usize) -> Result<(), AppError> {
    kv_write(root, "snapshot_keep_days", &snapshot_days.to_string())?;
    kv_write(root, "backup_keep", &backup_keep_n.to_string())?;
    Ok(())
}

/// 一键清理：快照按保留天数裁剪、备份按保留份数裁剪、回收站清空。
/// 返回 (释放字节数, 动作清单)。附件不自动清理（内容资产，仅面板可见）。
pub fn cleanup(root: &Path) -> Result<(u64, Vec<String>), AppError> {
    let mut freed = 0u64;
    let mut acts = Vec::new();

    // 快照：按天数（快照文件名 = {rel-slug}/{ts}.md；以 mtime 判龄）
    let keep_days = snapshot_keep_days(root);
    let snap_dir = root.join(".writer").join("snapshots");
    if snap_dir.is_dir() {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().saturating_sub(keep_days * 86400))
            .unwrap_or(0);
        if let Ok(rd) = std::fs::read_dir(&snap_dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Ok(sub) = std::fs::read_dir(&p) {
                        for f in sub.flatten() {
                            let fp = f.path();
                            if let Ok(m) = f.metadata() {
                                let ts = m
                                    .modified()
                                    .ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                if ts < cutoff {
                                    freed += m.len();
                                    let _ = std::fs::remove_file(&fp);
                                }
                            }
                        }
                    }
                }
            }
        }
        acts.push(format!("快照：已清理 {keep_days} 天前的版本"));
    }

    // 备份：按份数（时间戳字典序 = 时间序）
    let keep_n = backup_keep(root).max(1);
    let bdir = root.join(".writer").join("backups");
    if bdir.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&bdir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.starts_with("backup-") && n.ends_with(".zip"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        while names.len() > keep_n {
            let oldest = names.remove(0);
            if let Ok(m) = std::fs::metadata(bdir.join(&oldest)) {
                freed += m.len();
            }
            let _ = std::fs::remove_file(bdir.join(&oldest));
        }
        acts.push(format!("备份：已裁剪至最近 {keep_n} 份"));
    }

    // 回收站：清空（30 天红线仍建议用户先确认——UI 侧已二次确认）
    let trash = root.join(".writer").join("trash");
    if trash.is_dir() {
        let (n, b) = count_entries(&trash, |x| x != "index.json");
        if n > 0 {
            freed += b;
            if let Ok(rd) = std::fs::read_dir(&trash) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name == "index.json" {
                        continue;
                    }
                    let p = e.path();
                    if p.is_dir() {
                        let _ = std::fs::remove_dir_all(&p);
                    } else {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
            let _ = std::fs::remove_file(trash.join("index.json"));
            acts.push(format!("回收站：已清空 {n} 项"));
        }
    }
    crate::applog::info("", &format!("[storage] 一键清理释放 {freed} 字节"));
    Ok((freed, acts))
}

// ---------------------------------------------------------------------------
// 备份恢复向导（FR-SAV-05）
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct BackupEntry {
    pub name: String,
    pub ts: u64,
    pub docs: Vec<String>,
}

/// 列出备份内的 docs/ 文件清单（供向导预览/勾选）
pub fn list_backup(root: &Path, name: &str) -> Result<BackupEntry, AppError> {
    let path = root.join(".writer").join("backups").join(name);
    let f = std::fs::File::open(&path).map_err(|e| AppError::new("storage", e.to_string()))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| AppError::new("storage", format!("备份文件损坏：{e}")))?;
    let ts = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut docs = Vec::new();
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else { continue };
        let n = entry.name().to_string();
        if n.starts_with("docs/") && n.ends_with(".md") && !n.ends_with(".writer-sync.json") && !n.ends_with(".writer-stats.json") {
            docs.push(n);
        }
    }
    docs.sort();
    Ok(BackupEntry { name: name.to_string(), ts, docs })
}

/// 恢复：restore_all=true 整库（docs/ 全量替换）；否则只恢复 selected 内的篇目
///（重名生成「（恢复 N）」副本）。恢复前自动把当前状态再备份一份（红线）。
pub fn restore(
    root: &Path,
    name: &str,
    restore_all: bool,
    selected: &[String],
) -> Result<Vec<String>, AppError> {
    // 恢复前自动备份当前状态
    let guard = crate::backup::backup_now(root, false, None)
        .map_err(|e| AppError::new("storage", format!("恢复前自动备份失败，已中止：{}", e.message)))?;
    let path = root.join(".writer").join("backups").join(name);
    let f = std::fs::File::open(&path).map_err(|e| AppError::new("storage", e.to_string()))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| AppError::new("storage", format!("备份损坏：{e}")))?;

    let want: Box<dyn Fn(&str) -> bool> = if restore_all {
        Box::new(|n: &str| n.starts_with("docs/") || n.starts_with("attachments/"))
    } else {
        let sel: std::collections::HashSet<String> = selected.iter().cloned().collect();
        Box::new(move |n: &str| sel.contains(n))
    };

    let mut restored = Vec::new();
    {
        let lock = crate::library::Library::open_existing(root)?;
        let _g = lock.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        for i in 0..zip.len() {
            let Ok(mut entry) = zip.by_index(i) else { continue };
            let en = entry.name().to_string();
            if !want(&en) || entry.is_dir() {
                continue;
            }
            // 安全闸：目标不得越出库根（备份内相对路径直拼）
            let rel = en.trim_start_matches('/');
            if rel.split('/').any(|c| c.is_empty() || c == ".." || c.contains('\\')) {
                continue;
            }
            let mut bytes = Vec::new();
            if entry.read_to_end(&mut bytes).is_err() {
                continue;
            }
            let dest = root.join(&rel);
            let mut final_dest = dest.clone();
            if !restore_all && dest.exists() {
                // 单篇恢复重名 → 副本（不得静默覆盖现文）
                let stem = dest.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let ext = dest.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                let mut n = 2;
                while final_dest.exists() {
                    final_dest = dest.with_file_name(format!("{stem}（恢复 {n}）{ext}"));
                    n += 1;
                }
            }
            if let Some(parent) = final_dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| AppError::new("storage", e.to_string()))?;
            }
            atomic_write(&final_dest, &bytes)?;
            restored.push(rel.to_string());
        }
    }
    crate::applog::info(
        "",
        &format!("[restore] 从 {name} 恢复 {} 项（恢复前已自动备份）", restored.len()),
    );
    let _ = guard.name;
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_frees_trash_and_old_snapshots() {
        let root = std::env::temp_dir().join(format!("writer-storage-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["docs", ".writer/snapshots/doc1", ".writer/backups", ".writer/trash", "attachments"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // 新旧快照各一（旧 = mtime 设为 100 天前）
        std::fs::write(root.join(".writer/snapshots/doc1/new.md"), b"new").unwrap();
        let old = root.join(".writer/snapshots/doc1/old.md");
        std::fs::write(&old, b"old-content-1234").unwrap();
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86400);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(old_time).unwrap();
        // 回收站一项 + 备份两份（默认保留 7 → 不裁）
        std::fs::write(root.join(".writer/trash/100-x.md"), b"trashbytes").unwrap();
        std::fs::write(root.join(".writer/backups/backup-1.zip"), b"aaaa").unwrap();
        std::fs::write(root.join(".writer/backups/backup-2.zip"), b"bbbb").unwrap();

        let (freed, acts) = cleanup(&root).unwrap();
        assert!(freed > 0);
        assert!(!old.exists(), "旧快照应被清理");
        assert!(root.join(".writer/snapshots/doc1/new.md").exists(), "新快照保留");
        assert!(!root.join(".writer/trash/100-x.md").exists(), "回收站应清空");
        assert_eq!(acts.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// AC：备份 → 清空 docs/ → 向导整库恢复 → uuid 与文件全回来；单篇恢复重名生成副本
    #[test]
    fn restore_full_roundtrip_ac() {
        let root = std::env::temp_dir().join(format!("writer-storage-r-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs/卷一")).unwrap();
        std::fs::create_dir_all(root.join(".writer")).unwrap();
        std::fs::write(root.join("docs/卷一/a.md"), "---\nid: uuid-a\ntitle: A\n---\n正文").unwrap();
        std::fs::write(root.join("docs/b.md"), "---\nid: uuid-b\ntitle: B\n---\n正文").unwrap();
        // 建备份
        let name = crate::backup::backup_now(&root, false, None).unwrap().name;
        // 清空 docs/
        std::fs::remove_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        // 预览
        let entry = list_backup(&root, &name).unwrap();
        assert_eq!(entry.docs.len(), 2, "备份应含两篇（清单文件不计）");
        // 整库恢复
        let restored = restore(&root, &name, true, &[]).unwrap();
        assert!(restored.len() >= 2);
        let a = std::fs::read_to_string(root.join("docs/卷一/a.md")).unwrap();
        assert!(a.contains("uuid-a"), "uuid 必须原样恢复");
        assert!(std::fs::read_to_string(root.join("docs/b.md")).unwrap().contains("uuid-b"));
        // 单篇恢复到已有文件 → 生成副本不覆盖
        let r2 = restore(&root, &name, false, &["docs/b.md".to_string()]).unwrap();
        assert_eq!(r2.len(), 1);
        assert!(root.join("docs/b（恢复 2）.md").exists(), "重名应生成副本");
        let _ = std::fs::remove_dir_all(&root);
    }
}
