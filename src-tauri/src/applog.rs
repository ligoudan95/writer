//! 本地日志（FR-SET-03）：`.writer/logs/writer-YYYYMMDD.log`，滚动保留 7 天。
//! 前端与 Rust 落同一文件，按 request-id 关联（前端经 log_write 命令写入）。

use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

static LOG_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 设置当前库根（库打开/切换时调用）；日志目录随库
pub fn set_library_root(root: &std::path::Path) {
    let dir = root.join(".writer").join("logs");
    let _ = fs::create_dir_all(&dir);
    prune_old(&dir);
    *LOG_ROOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(root.to_path_buf());
}

pub fn clear_library_root() {
    *LOG_ROOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Unix → YYYYMMDD（UTC，日志文件名用途）
fn day_stamp(t: u64) -> String {
    let (y, m, d) = civil_from_days((t / 86400) as i64);
    format!("{y:04}{m:02}{d:02}")
}

/// YYYYMMDD → epoch 天数（civil_from_days 的逆运算；7 天滚动按天比较，
/// 不能用 YYYYMMDD 数值相减——跨月会误删新月头几天的日志）
fn days_from_stamp(s: &str) -> Option<i64> {
    if s.len() != 8 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: u32 = s[4..6].parse().ok()?;
    let d: u32 = s[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe as i64 - 719_468)
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

/// 删除 7 天前的日志文件（按文件名日期换算回天数比较）
fn prune_old(dir: &std::path::Path) {
    let today = (now_secs() / 86400) as i64;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(d) = name
                .strip_prefix("writer-")
                .and_then(|s| s.strip_suffix(".log"))
                .and_then(days_from_stamp)
            else {
                continue;
            };
            if today - d > 6 {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// 写一条日志。request_id 为空时记 "-"；level: info/warn/error
pub fn write(level: &str, request_id: &str, message: &str) {
    let guard = LOG_ROOT.lock().unwrap_or_else(|e| e.into_inner());
    let Some(root) = guard.as_ref() else { return };
    let dir = root.join(".writer").join("logs");
    let t = now_secs();
    let secs = t % 86400;
    let line = format!(
        "{} {:02}:{:02}:{:02} [{}] [{}] {}\n",
        day_stamp(t),
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
        level.to_uppercase(),
        if request_id.is_empty() { "-" } else { request_id },
        message.replace('\n', " ")
    );
    let path = dir.join(format!("writer-{}.log", day_stamp(t)));
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn info(request_id: &str, message: &str) {
    write("info", request_id, message);
}

pub fn warn(request_id: &str, message: &str) {
    write("warn", request_id, message);
}

pub fn error(request_id: &str, message: &str) {
    write("error", request_id, message);
}

/// 日志目录（"打开日志目录"入口用）
pub fn logs_dir() -> Option<PathBuf> {
    LOG_ROOT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|r| r.join(".writer").join("logs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_stamp_format() {
        assert_eq!(day_stamp(0), "19700101");
        assert_eq!(day_stamp(1_900_000_000), "20300317");
    }

    #[test]
    fn stamp_day_roundtrip_and_7day_window() {
        assert_eq!(days_from_stamp("19700101"), Some(0));
        assert_eq!(days_from_stamp("20300317"), Some(1_900_000_000 / 86400));
        // 跨月窗口：10-31 与 11-06 相差 6 天（保留），11-07 为 7 天（删除）
        let a = days_from_stamp("20261031").unwrap();
        let b = days_from_stamp("20261106").unwrap();
        let c = days_from_stamp("20261107").unwrap();
        assert_eq!(b - a, 6);
        assert_eq!(c - a, 7);
        assert_eq!(days_from_stamp("20261301"), None);
    }
}
