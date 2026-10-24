//! 本地滚动备份（FR-SAV-04）与诊断包导出（FR-SET-03）。
//!
//! 备份范围：docs/、.writer/meta.db、.writer/config.json、.writer/snapshots/ 必含
//! （保护快照、统计、AI 对话等仅本地数据）；attachments/ 默认排除（可含）；
//! logs/、trash/、sync_state.db、.lock 不含（无保留价值或可重建/瞬态）。
//! 保留最近 7 份 zip；每日启动检查触发。

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::library::AppError;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn backups_dir(root: &Path) -> PathBuf {
    root.join(".writer").join("backups")
}

fn stamp(t: u64) -> String {
    let days = t / 86400;
    let secs = t % 86400;
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

fn add_dir_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    base: &Path,
    rel_prefix: &str,
    root_for_strip: &Path,
) -> Result<(), AppError> {
    add_dir_inner(zip, base, rel_prefix, root_for_strip, 0)
}

fn add_dir_inner(
    zip: &mut zip::ZipWriter<std::fs::File>,
    base: &Path,
    rel_prefix: &str,
    root_for_strip: &Path,
    depth: usize,
) -> Result<(), AppError> {
    if depth > 32 {
        return Ok(()); // 深度上限防 junction/符号链接环
    }
    let _ = rel_prefix;
    let Ok(rd) = fs::read_dir(base) else { return Ok(()) };
    for e in rd.flatten() {
        let p = e.path();
        let rel = p
            .strip_prefix(root_for_strip)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        let name = if rel_prefix.is_empty() { rel.clone() } else { format!("{rel_prefix}/{rel}") };
        // 名字统一用相对库根路径（恢复时直接展开）
        if p.is_dir() {
            add_dir_to_zip(zip, &p, "", root_for_strip)?;
        } else if let Ok(bytes) = fs::read(&p) {
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file(name, opts)
                .map_err(|e| AppError::new("io", e.to_string()))?;
            zip.write_all(&bytes)?;
        }
    }
    Ok(())
}

fn add_file_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    path: &Path,
    name: &str,
) -> Result<(), AppError> {
    let bytes = fs::read(path).map_err(|e| AppError::new("io", e.to_string()))?;
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(name.to_string(), opts)
        .map_err(|e| AppError::new("io", e.to_string()))?;
    zip.write_all(&bytes)?;
    Ok(())
}

/// 创建一份备份 zip（临时文件 + rename 原子落位；失败清理半成品），
/// 返回文件名；prune 保留最近 7 份。
/// meta.db 处于 WAL 模式：裸拷主库会漏 `-wal` 中的最近事务——备份前先
/// checkpoint(TRUNCATE) 把 WAL 落回主库（FR-SAV-04：AI 对话等仅本地数据必须完整）
fn create_backup(root: &Path, include_attachments: bool) -> Result<String, AppError> {
    let dir = backups_dir(root);
    fs::create_dir_all(&dir)?;
    // 同秒重名碰撞：文件名秒级时间戳，一秒内连做两份备份（如恢复向导的
    // "恢复前自动备份"）会静默互覆——同名时追加 -2/-3 唯一化
    let mut name = format!("backup-{}.zip", stamp(now_secs()));
    {
        let mut n = 2;
        while dir.join(&name).exists() || dir.join(format!("{name}.tmp")).exists() {
            name = format!("backup-{}-{n}.zip", stamp(now_secs()));
            n += 1;
        }
    }
    let path = dir.join(&name);
    let tmp = dir.join(format!("{name}.tmp"));
    let result = (|| -> Result<(), AppError> {
        checkpoint_meta_db(root);
        let file = fs::File::create(&tmp)?;
        let mut zip = zip::ZipWriter::new(file);
        add_dir_to_zip(&mut zip, &root.join("docs"), "", root)?;
        for must in ["meta.db", "config.json"] {
            let p = root.join(".writer").join(must);
            if p.exists() {
                add_file_to_zip(&mut zip, &p, &format!(".writer/{must}"))?;
            }
        }
        add_dir_to_zip(&mut zip, &root.join(".writer").join("snapshots"), "", root)?;
        if include_attachments {
            add_dir_to_zip(&mut zip, &root.join("attachments"), "", root)?;
        }
        zip.finish().map_err(|e| AppError::new("io", e.to_string()))?;
        fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp); // 失败不留半成品（否则计入 7 份配额且展示为有效备份）
        result?;
    }
    prune(root);
    Ok(name)
}

/// WAL checkpoint：把 meta.db-wal 落回主库；失败不阻塞备份（拿到稍旧快照优于无备份）
fn checkpoint_meta_db(root: &Path) {
    let _ = crate::db::MetaDb::open(root).and_then(|db| db.checkpoint_wal());
}

fn prune(root: &Path) {
    let dir = backups_dir(root);
    let Ok(rd) = fs::read_dir(&dir) else { return };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("backup-") && n.ends_with(".zip"))
        .collect();
    names.sort(); // 时间戳字典序 = 时间序
    while names.len() > 7 {
        let oldest = names.remove(0);
        let _ = fs::remove_file(dir.join(oldest));
    }
}

#[derive(serde::Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub size: u64,
    pub ts: u64,
}

pub fn list_backups(root: &Path) -> Vec<BackupInfo> {
    let dir = backups_dir(root);
    let Ok(rd) = fs::read_dir(&dir) else { return vec![] };
    let mut out: Vec<BackupInfo> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !(name.starts_with("backup-") && name.ends_with(".zip")) {
                return None;
            }
            let size = e.metadata().ok().map(|m| m.len()).unwrap_or(0);
            let ts = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Some(BackupInfo { name, size, ts })
        })
        .collect();
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    out
}

/// 备份到库内（滚动）并可选同步复制一份到用户指定位置（"一键备份到任意位置"）
pub fn backup_now(root: &Path, include_attachments: bool, copy_to: Option<&str>) -> Result<BackupInfo, AppError> {
    let name = create_backup(root, include_attachments)?;
    if let Some(dest) = copy_to {
        let src = backups_dir(root).join(&name);
        let dest_path = if dest.ends_with(".zip") {
            PathBuf::from(dest)
        } else {
            PathBuf::from(dest).join(&name)
        };
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&src, &dest_path)?;
    }
    let size = fs::metadata(backups_dir(root).join(&name))
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(BackupInfo { name, size, ts: now_secs() })
}

/// 每日检查：最近一份备份超过 24h → 自动做一份（启动时后台调用，失败仅记日志）
pub fn maybe_daily_backup(root: &Path) {
    let latest = list_backups(root).first().map(|b| b.ts).unwrap_or(0);
    if now_secs().saturating_sub(latest) >= 86_400 {
        if let Err(e) = create_backup(root, false) {
            crate::applog::error("", &format!("每日自动备份失败：{}", e.message));
        } else {
            crate::applog::info("", "每日自动备份完成");
        }
    }
}

/// 诊断包导出（FR-SET-03，脱敏）：日志 + 库配置（凭据不入包——本来就在 DPAPI/远端）+
/// 库结构统计 + 应用配置（剥离 lastLibrary 之外无敏感；WebDAV 用户名打码）。
/// 绝不包含：正文内容、API Key、WebDAV 密码
pub fn export_diagnostics(root: &Path, app_config_path: Option<&Path>, dest: &str) -> Result<(), AppError> {
    let file = fs::File::create(dest)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // 1. 日志（近 7 天全部）
    let logs = root.join(".writer").join("logs");
    if logs.is_dir() {
        if let Ok(rd) = fs::read_dir(&logs) {
            for e in rd.flatten() {
                if let Ok(bytes) = fs::read(e.path()) {
                    let n = e.file_name().to_string_lossy().to_string();
                    zip.start_file(format!("logs/{n}"), opts)
                        .map_err(|e| AppError::new("io", e.to_string()))?;
                    zip.write_all(&bytes)?;
                }
            }
        }
    }

    // 2. 同步历史与状态摘要（路径/动作/流量，无内容）
    if let Ok(db) = crate::sync::SyncDb::open(root) {
        let rows = db.log_list(500);
        let mut txt = String::from("time\tfile\tdirection\tbytes\taction\tresult\n");
        for r in rows {
            txt.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\n", r.time, r.file, r.direction, r.bytes, r.action, r.result));
        }
        zip.start_file("sync-history.tsv", opts)
            .map_err(|e| AppError::new("io", e.to_string()))?;
        zip.write_all(txt.as_bytes())?;
    }

    // 3. 库结构统计
    let mut stats = String::new();
    let s = crate::sync::library_summary(root);
    stats.push_str(&format!("documents: {}\n", s.docs));
    stats.push_str(&format!("attachments: {}\n", s.attachments));
    stats.push_str(&format!("docs_missing_id: {}\n", s.docs_missing_id));
    zip.start_file("library-stats.txt", opts)
        .map_err(|e| AppError::new("io", e.to_string()))?;
    zip.write_all(stats.as_bytes())?;

    // 4. 应用配置（脱敏：WebDAV 用户名仅保留首字符）
    if let Some(p) = app_config_path {
        if let Ok(bytes) = fs::read(p) {
            let redacted = redact_config(&String::from_utf8_lossy(&bytes));
            zip.start_file("app-config.json", opts)
                .map_err(|e| AppError::new("io", e.to_string()))?;
            zip.write_all(redacted.as_bytes())?;
        }
    }

    zip.finish().map_err(|e| AppError::new("io", e.to_string()))?;
    Ok(())
}

/// 简易脱敏：username 值保留首字符 + ***；其余键值原样（配置不含密钥/密码）
fn redact_config(s: &str) -> String {
    // 仅同步配置里有 username；应用 config.json（lastLibrary）本就无敏感
    if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(obj) = v.as_object_mut() {
            for (_k, val) in obj.iter_mut() {
                if let Some(cfg) = val.as_object_mut() {
                    if let Some(u) = cfg.get_mut("username") {
                        if let Some(name) = u.as_str().map(|n| n.to_string()) {
                            let masked: String = name
                                .chars()
                                .enumerate()
                                .map(|(i, c)| if i == 0 { c } else { '*' })
                                .collect();
                            *u = serde_json::Value::String(masked);
                        }
                    }
                }
            }
        }
        return serde_json::to_string_pretty(&v).unwrap_or_else(|_| s.to_string());
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_format() {
        assert_eq!(stamp(0), "19700101-000000");
    }

    #[test]
    fn backup_roundtrip_and_prune() {
        let dir = std::env::temp_dir().join(format!("writer-backup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("docs").join("章节")).unwrap();
        fs::write(dir.join("docs").join("章节").join("a.md"), "内容内容").unwrap();
        fs::create_dir_all(dir.join(".writer").join("logs")).unwrap();
        fs::write(dir.join(".writer").join("logs").join("writer-20260101.log"), "log").unwrap();
        fs::create_dir_all(dir.join(".writer").join("trash")).unwrap();
        fs::write(dir.join(".writer").join("trash").join("垃圾.md"), "trash").unwrap();

        let info = backup_now(&dir, false, None).unwrap();
        assert!(info.name.starts_with("backup-"));
        let list = list_backups(&dir);
        assert_eq!(list.len(), 1);

        // 校验内容：docs 进包、logs/trash 不进包
        let f = fs::File::open(backups_dir(&dir).join(&info.name)).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let names: Vec<String> = (0..z.len()).filter_map(|i| z.by_index(i).ok().and_then(|f| Some(f.name().to_string()))).collect();
        assert!(names.iter().any(|n| n.contains("章节") && n.ends_with("a.md")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("trash")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("logs/")), "{names:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
