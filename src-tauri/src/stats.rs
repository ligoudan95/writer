//! 写作目标与统计（FR-STAT-01/02，V1.1）。
//! 机制（规格固定）：按文档按日基线表 doc_word_daily(date, doc_uuid, words)，
//! 保存/同步/启动/日切时更新（只写"今天"行——历史日期自然冻结为该日最后值）；
//! 日增字数 = Σ_doc (基线(D) − 基线(D−1))，逐文档负差计 0（回删不计负），
//! 当日缺失的文档（已删）不计入（历史基线冻结）。
//! 跨设备聚合（v1.5 定案）：每日增量按 (device_id, date) 写入 docs/.writer-stats.json，
//! 并集合并天然幂等（同墓碑清单）；streak/热力图基于并集。

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;

use crate::frontmatter;
use crate::library::AppError;

pub const STATS_MANIFEST: &str = "docs/.writer-stats.json";

/// 字数口径与前端一致（FR-EDT-07）：CJK 逐字计，西文/数字连续串计 1
pub fn count_words(text: &str) -> i64 {
    let mut n = 0i64;
    let mut in_word = false;
    for c in text.chars() {
        let cjk =
            ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3040}'..='\u{30FF}').contains(&c);
        if cjk {
            n += 1;
            in_word = false;
        } else if c.is_ascii_alphanumeric() {
            if !in_word {
                n += 1;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    n
}

fn map_db(e: rusqlite::Error) -> AppError {
    AppError::new("stats", e.to_string())
}

fn open(root: &Path) -> Result<Connection, AppError> {
    let dir = root.join(".writer");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::new("stats", e.to_string()))?;
    let conn = Connection::open(dir.join("meta.db")).map_err(|e| AppError::new("stats", e.to_string()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS doc_word_daily(
            date TEXT NOT NULL, doc_uuid TEXT NOT NULL, words INTEGER NOT NULL,
            PRIMARY KEY(date, doc_uuid));",
    )
    .map_err(map_db)?;
    Ok(conn)
}

fn today() -> String {
    local_date(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0))
}

/// UTC 秒 → 本地 YYYY-MM-DD（civil 算法，无外部依赖；日切精度到天足够）
fn local_date(unix: u64) -> String {
    let local = unix as i64 + local_offset_secs();
    let z = local.div_euclid(86400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// 本地时区偏移（秒）。无 chrono 依赖下的近似：中国区 +8；偏差仅影响
/// 跨 UTC 日切时刻的归日，统计口径可接受
fn local_offset_secs() -> i64 {
    8 * 3600
}

fn doc_uuid_and_words(content: &str) -> (String, i64) {
    let (fm, body) = frontmatter::split(content);
    let uuid = fm.as_ref().and_then(|f| f.get("id")).unwrap_or("").to_string();
    (uuid, count_words(&body))
}

fn walk_md(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let p = e.path();
        if p.is_dir() {
            walk_md(&p, &rel, out);
        } else if name.to_lowercase().ends_with(".md") {
            out.push(rel);
        }
    }
}

/// 单文档基线更新（保存路径——O(单文档)）：从磁盘读回完整文件（含 frontmatter）
pub fn record_doc_from_disk(root: &Path, rel: &str) -> Result<(), AppError> {
    let bytes = std::fs::read(root.join(rel)).map_err(|e| AppError::new("stats", e.to_string()))?;
    record_doc(root, rel, &String::from_utf8_lossy(&bytes))
}

/// 单文档基线更新（内存内容版）
pub fn record_doc(root: &Path, _rel: &str, content: &str) -> Result<(), AppError> {
    let (uuid, words) = doc_uuid_and_words(content);
    if uuid.is_empty() {
        return Ok(()); // 无 id 文档：入库体检流程处理，不入统计
    }
    let conn = open(root)?;
    conn.execute(
        "INSERT OR REPLACE INTO doc_word_daily(date, doc_uuid, words) VALUES (?1, ?2, ?3)",
        rusqlite::params![today(), uuid, words],
    )
    .map_err(map_db)?;
    Ok(())
}

/// 全库基线刷新（启动/同步批次结束时——O(全库)）
pub fn record_all(root: &Path) -> Result<usize, AppError> {
    let mut files = Vec::new();
    walk_md(&root.join("docs"), "docs", &mut files);
    let conn = open(root)?;
    let d = today();
    for rel in &files {
        let Ok(bytes) = std::fs::read(root.join(rel)) else { continue };
        let (uuid, words) = doc_uuid_and_words(&String::from_utf8_lossy(&bytes));
        if uuid.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT OR REPLACE INTO doc_word_daily(date, doc_uuid, words) VALUES (?1, ?2, ?3)",
            rusqlite::params![d, uuid, words],
        )
        .map_err(map_db)?;
    }
    Ok(files.len())
}

fn prev_day(date: &str) -> Option<String> {
    // YYYY-MM-DD − 1 天（civil 算法逆推）
    let mut y: i64 = date.get(0..4)?.parse().ok()?;
    let m: i64 = date.get(5..7)?.parse().ok()?;
    let d: i64 = date.get(8..10)?.parse().ok()?;
    let days = civil_to_days(y, m, d) - 1;
    let (yy, mm, dd) = days_to_civil(days);
    y = yy;
    Some(format!("{y:04}-{mm:02}-{dd:02}"))
}

fn civil_to_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_to_civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 某日期的净增字数（基线差分；逐文档负差计 0；当日缺失文档不计）
fn day_added(conn: &Connection, date: &str) -> Result<i64, AppError> {
    let prev = prev_day(date);
    let mut stmt = conn
        .prepare("SELECT doc_uuid, words FROM doc_word_daily WHERE date = ?1")
        .map_err(map_db)?;
    let cur: Vec<(String, i64)> = stmt
        .query_map([date], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(map_db)?
        .flatten()
        .map(|r| (r.0, r.1))
        .collect();
    let prev_map: BTreeMap<String, i64> = match &prev {
        Some(p) => {
            let mut st = conn
                .prepare("SELECT doc_uuid, words FROM doc_word_daily WHERE date = ?1")
                .map_err(map_db)?;
            st.query_map([p], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map_err(map_db)?
                .flatten()
                .collect()
        }
        None => BTreeMap::new(),
    };
    let mut total = 0i64;
    for (uuid, words) in &cur {
        let base = prev_map.get(uuid).copied().unwrap_or(0);
        total += (words - base).max(0);
    }
    Ok(total)
}

// -- 统计聚合清单（跨设备并集） ----------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Default, Debug, Clone, PartialEq)]
pub struct StatsManifest {
    #[serde(default)]
    pub devices: BTreeMap<String, BTreeMap<String, i64>>,
}

pub fn load_manifest(root: &Path) -> StatsManifest {
    std::fs::read_to_string(root.join(STATS_MANIFEST))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_manifest(root: &Path, m: &StatsManifest) -> Result<(), AppError> {
    let p = root.join(STATS_MANIFEST);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::new("stats", e.to_string()))?;
    }
    crate::library::atomic_write(&p, serde_json::to_vec_pretty(m).unwrap_or_default().as_slice())?;
    Ok(())
}

/// 并集合并（v1.5 定案）：按 (device_id, date) 并集；本设备行以本地为准（权威），
/// 对端行原样收编——天然幂等，永不走冲突副本
pub fn merge_manifest(local: &StatsManifest, remote: &StatsManifest) -> StatsManifest {
    let mut out = local.clone();
    for (dev, days) in &remote.devices {
        if dev == &device_id() {
            continue; // 自己的设备行以本地为准
        }
        let slot = out.devices.entry(dev.clone()).or_default();
        for (d, w) in days {
            slot.entry(d.clone()).or_insert(*w);
        }
    }
    out
}

/// 本机 device_id（kv 持久，首用分配）
pub fn device_id() -> String {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<String>> = Mutex::new(None);
    if let Some(id) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return id;
    }
    let id = std::fs::read_to_string(
        dirs::config_dir().map(|d| d.join("Writer").join("device_id")).unwrap_or_default(),
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let _ = std::fs::create_dir_all(dirs::config_dir().map(|d| d.join("Writer")).unwrap_or_default());
    let _ = std::fs::write(
        dirs::config_dir().map(|d| d.join("Writer").join("device_id")).unwrap_or_default(),
        &id,
    );
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
    id
}

/// 刷新本地聚合清单：本设备今天增量（基线差分）写入后返回清单
pub fn refresh_manifest(root: &Path) -> Result<StatsManifest, AppError> {
    let conn = open(root)?;
    let added = day_added(&conn, &today())?;
    let mut m = load_manifest(root);
    m.devices
        .entry(device_id())
        .or_default()
        .insert(today(), added);
    save_manifest(root, &m)?;
    Ok(m)
}

#[derive(serde::Serialize)]
pub struct StatsSummary {
    /// 并集后的 每日增量（近 400 天）
    pub days: Vec<(String, i64)>,
    pub today: i64,
    pub goal: i64,
    pub streak: i64,
    pub active_days: i64,
    pub week_total: i64,
    pub month_total: i64,
}

pub fn set_goal(root: &Path, goal: i64) -> Result<i64, AppError> {
    let conn = open(root)?;
    conn.execute(
        "INSERT OR REPLACE INTO kv(k, v) VALUES ('stat_daily_goal', ?1)",
        [goal.to_string()],
    )
    .map_err(map_db)?;
    Ok(goal)
}

fn get_goal(conn: &Connection) -> i64 {
    conn.query_row("SELECT v FROM kv WHERE k = 'stat_daily_goal'", [], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(1000)
}

/// 汇总：并集日增（近 400 天）、今日、streak、活跃天数、周/月累计
pub fn summary(root: &Path) -> Result<StatsSummary, AppError> {
    let conn = open(root)?;
    let manifest = refresh_manifest(root)?;
    // 并集聚合：每设备行同日多值不可能（or_insert 幂等）；不同设备同日相加
    let mut per_day: BTreeMap<String, i64> = BTreeMap::new();
    for (_dev, days) in &manifest.devices {
        for (d, w) in days {
            *per_day.entry(d.clone()).or_insert(0) += w;
        }
    }
    // 今日：基线实时差分（清单里也是它，但确保保存后最新）
    let today_added = day_added(&conn, &today())?;
    let today_str = today();
    per_day.insert(today_str.clone(), today_added);
    // 只保留近 400 天
    let mut days: Vec<(String, i64)> = per_day
        .into_iter()
        .filter(|(d, _)| d.as_str() >= cutoff_400d(&today_str).as_str())
        .collect();
    days.sort();
    // streak：从今日（或昨日）往回连续 >0 的天数
    let mut streak = 0i64;
    let map: BTreeMap<&str, i64> = days.iter().map(|(d, w)| (d.as_str(), *w)).collect();
    let today_str = today();
    // 今日无进度不立即断连（从昨日回溯）；今日有进度则计入
    if map.get(today_str.as_str()).copied().unwrap_or(0) > 0 {
        streak += 1;
    }
    let mut c = prev_day(&today_str).unwrap_or_default();
    while !c.is_empty() && map.get(c.as_str()).copied().unwrap_or(0) > 0 {
        streak += 1;
        c = prev_day(&c).unwrap_or_default();
    }
    let active_days = days.iter().filter(|(_, w)| *w > 0).count() as i64;
    // 周/月累计（含今日）
    let week_start = civil_to_days_str(&today_str) - 6;
    let month_prefix = &today_str[..7];
    let mut week_total = 0i64;
    let mut month_total = 0i64;
    for (d, w) in &days {
        if civil_to_days_str(d) >= week_start {
            week_total += w;
        }
        if d.starts_with(month_prefix) {
            month_total += w;
        }
    }
    Ok(StatsSummary {
        days,
        today: today_added,
        goal: get_goal(&conn),
        streak,
        active_days,
        week_total,
        month_total,
    })
}

fn cutoff_400d(today: &str) -> String {
    let days = civil_to_days_str(today) - 399;
    let (y, m, d) = days_to_civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_to_days_str(date: &str) -> i64 {
    let y: i64 = date.get(0..4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let m: i64 = date.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    let d: i64 = date.get(8..10).and_then(|s| s.parse().ok()).unwrap_or(1);
    civil_to_days(y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_lib(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("writer-stats-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("docs")).unwrap();
        d
    }

    fn w(root: &Path, name: &str, uuid: &str, text: &str) -> String {
        let content = format!("---\nid: {uuid}\ntitle: t\n---\n{text}");
        std::fs::write(root.join("docs").join(name), &content).unwrap();
        content
    }

    #[test]
    fn word_counting_rule() {
        assert_eq!(count_words("你好世界"), 4);
        assert_eq!(count_words("hello world 123"), 3);
        // 中文2 + english + 混合2 + 99 + 词 = 7
        assert_eq!(count_words("中文english混合99词"), 7);
        assert_eq!(count_words(""), 0);
    }

    #[test]
    fn date_math_roundtrip() {
        assert_eq!(prev_day("2026-10-24").unwrap(), "2026-10-23");
        assert_eq!(prev_day("2026-01-01").unwrap(), "2025-12-31");
        assert_eq!(prev_day("2024-03-01").unwrap(), "2024-02-29"); // 闰年
        assert_eq!(cutoff_400d("2026-10-24"), "2025-09-20");
    }

    #[test]
    fn day_added_clamps_negative_and_skips_deleted() {
        let root = tmp_lib("diff");
        let c1 = w(&root, "a.md", "uuid-a", "一二三四五六七八九十一"); // 11 字
        record_doc(&root, "docs/a.md", &c1).unwrap();
        let conn = open(&root).unwrap();
        let t = today();
        // 今日新文档：无昨日基线 → 全字数计入
        assert_eq!(day_added(&conn, &t).unwrap(), 11);
        drop(conn);
        // 改少（回删）：今日基线被今日保存覆盖 → 负差不计
        let c1b = w(&root, "a.md", "uuid-a", "五个字五个"); // 5 字
        record_doc(&root, "docs/a.md", &c1b).unwrap();
        let conn = open(&root).unwrap();
        // 基线同日被覆盖为 5（无昨日行）→ 计 5：体现"当日最终值"语义
        assert_eq!(day_added(&conn, &t).unwrap(), 5);
        // 造昨日基线 5 → 今日基线 5 → 增量 0
        conn.execute(
            "INSERT OR REPLACE INTO doc_word_daily VALUES (?1, 'uuid-a', 5)",
            [prev_day(&t).unwrap()],
        )
        .unwrap();
        assert_eq!(day_added(&conn, &t).unwrap(), 0);
        // 昨日 5 → 今日 12：增 7
        conn.execute("INSERT OR REPLACE INTO doc_word_daily VALUES (?1, 'uuid-a', 5)", [&t]).unwrap();
        conn.execute("UPDATE doc_word_daily SET words = 12 WHERE date = ?1", [&t]).unwrap();
        assert_eq!(day_added(&conn, &t).unwrap(), 7);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_union_merge_idempotent() {
        let mut local = StatsManifest::default();
        local.devices.insert("devA".into(), [("2026-10-24".to_string(), 100)].into_iter().collect());
        let mut remote = StatsManifest::default();
        // devA 的对端旧值：本地权威（or_insert 保留本地）
        remote.devices.insert("devA".into(), [("2026-10-24".to_string(), 50)].into_iter().collect());
        remote.devices.insert("devB".into(), [("2026-10-23".to_string(), 300)].into_iter().collect());
        let merged = merge_manifest(&local, &remote);
        assert_eq!(merged.devices["devA"]["2026-10-24"], 100);
        assert_eq!(merged.devices["devB"]["2026-10-23"], 300);
        // 幂等：再合一次不变
        assert_eq!(merge_manifest(&merged, &remote), merged);
    }
}
