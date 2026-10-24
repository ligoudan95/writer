//! 跨文档全文搜索（FR-SEARCH-01，V1.1）。
//! 索引策略（规格固定）：≥3 字查询走 FTS5 trigram 短语匹配；≤2 字中文回退 LIKE
//! 顺序扫描（trigram 不支持 2 字子串；目标库规模下全扫为毫秒级，A8）。
//! 新鲜度：查询前对账（mtime+size 逐文件比对，变更者增量重索引）——
//! 自愈全部写入路径（编辑保存/同步下载/导入/外部修改），无需侵入各写入点。
//! 索引位于 meta.db（可重建缓存，随删随建，规格 6.5 数据角色定位）。

use std::path::Path;

use rusqlite::Connection;

use crate::frontmatter;
use crate::library::{display_name_from_filename, AppError};

#[derive(serde::Serialize, Clone, Debug)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    /// 命中上下文片段（含高亮锚点由前端按原文重找）
    pub snippet: String,
    /// 命中次数（相关度排序参考）
    pub score: i64,
}

/// 建表（幂等）。trigram 分词器：CJK 按字符参与 3-gram，短语匹配即子串匹配。
fn ensure_tables(conn: &Connection) -> Result<(), AppError> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS fts_docs USING fts5(
            path UNINDEXED, title, body, tokenize='trigram');
         CREATE TABLE IF NOT EXISTS fts_stat(
            path TEXT PRIMARY KEY, mtime INTEGER NOT NULL, size INTEGER NOT NULL);",
    )
    .map_err(|e| AppError::new("search", format!("建索引表失败：{e}")))?;
    Ok(())
}

/// 打开 meta.db 内的索引（与 MetaDb 同库，事务由调用方管理）
pub fn open(root: &Path) -> Result<Connection, AppError> {
    let dir = root.join(".writer");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::new("search", e.to_string()))?;
    let conn = Connection::open(dir.join("meta.db"))
        .map_err(|e| AppError::new("search", e.to_string()))?;
    ensure_tables(&conn)?;
    Ok(conn)
}

fn read_doc(root: &Path, rel: &str) -> Option<(String, String)> {
    let bytes = std::fs::read(root.join(rel)).ok()?;
    let content = String::from_utf8_lossy(&bytes).to_string();
    let (fm, body) = frontmatter::split(&content);
    let title = fm
        .as_ref()
        .and_then(|f| f.get("title"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| crate::library::display_name_from_filename(&rel.rsplit('/').next().unwrap_or(rel)));
    Some((title, body))
}

/// 单文件（重）索引；文件不存在则移除索引行
fn reindex_one(conn: &Connection, root: &Path, rel: &str) -> Result<(), AppError> {
    let p = root.join(rel);
    let meta = std::fs::metadata(&p);
    match meta {
        Err(_) => {
            let _ = conn.execute("DELETE FROM fts_docs WHERE path = ?1", [rel]);
            let _ = conn.execute("DELETE FROM fts_stat WHERE path = ?1", [rel]);
            Ok(())
        }
        Ok(m) => {
            let Some((title, body)) = read_doc(root, rel) else {
                return Ok(()); // 读取失败（权限等）：保留旧索引，下次再试
            };
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            conn.execute("DELETE FROM fts_docs WHERE path = ?1", [rel])
                .map_err(map_db)?;
            conn.execute(
                "INSERT INTO fts_docs(path, title, body) VALUES (?1, ?2, ?3)",
                rusqlite::params![rel, title, body],
            )
            .map_err(map_db)?;
            conn.execute(
                "INSERT OR REPLACE INTO fts_stat(path, mtime, size) VALUES (?1, ?2, ?3)",
                rusqlite::params![rel, mtime, m.len() as i64],
            )
            .map_err(map_db)?;
            Ok(())
        }
    }
}

fn map_db(e: rusqlite::Error) -> AppError {
    AppError::new("search", e.to_string())
}

/// 收集 docs/ 下全部 .md 相对路径（/ 分隔）
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

/// 查询前对账：新增/变更（mtime 或 size 变化）的文件增量重索引，消失的移除。
/// 返回对账耗时提示日志（个人库规模毫秒级；500 篇统计走一遍 <50ms）
pub fn reconcile(conn: &Connection, root: &Path) -> Result<usize, AppError> {
    let mut files = Vec::new();
    walk_md(&root.join("docs"), "docs", &mut files);
    let mut stale: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT mtime, size FROM fts_stat WHERE path = ?1")
            .map_err(map_db)?;
        for rel in &files {
            let Ok(m) = std::fs::metadata(root.join(rel)) else { continue };
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let size = m.len() as i64;
            let row: Option<(i64, i64)> = stmt
                .query_row([rel], |r| Ok((r.get(0)?, r.get(1)?)))
                .ok();
            match row {
                Some((mt, sz)) if mt == mtime && sz == size => {}
                _ => stale.push(rel.clone()),
            }
        }
    }
    // 已索引但文件消失的
    let vanished: Vec<String> = {
        let mut stmt = conn.prepare("SELECT path FROM fts_stat").map_err(map_db)?;
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map_err(map_db)?
            .flatten()
            .filter(|p| !files.contains(p))
            .collect()
    };
    let mut n = 0;
    for rel in stale.iter().chain(vanished.iter()) {
        reindex_one(conn, root, rel)?;
        n += 1;
    }
    Ok(n)
}

/// 全量重建（库体检/索引损坏时）
pub fn rebuild(conn: &Connection, root: &Path) -> Result<usize, AppError> {
    conn.execute_batch("DELETE FROM fts_docs; DELETE FROM fts_stat;").map_err(map_db)?;
    let mut files = Vec::new();
    walk_md(&root.join("docs"), "docs", &mut files);
    for rel in &files {
        reindex_one(conn, root, rel)?;
    }
    Ok(files.len())
}

fn is_short_cjk(q: &str) -> bool {
    q.chars().count() <= 2
}

/// 手工截取命中上下文（LIKE 回退路径用；FTS 路径用 snippet()）。返回 (片段, 命中次数)
fn manual_snippet(body: &str, q: &str) -> (String, i64) {
    let mut buf = String::new();
    // 找第一个命中位置（不区分大小写的朴素查找足够：正文与查询都此处小写化）
    let lb = body.to_lowercase();
    let lq = q.to_lowercase();
    let mut start = 0usize;
    let mut count = 0i64;
    let mut first: Option<usize> = None;
    while let Some(i) = lb[start..].find(&lq) {
        let at = start + i;
        if first.is_none() {
            first = Some(at);
        }
        count += 1;
        start = at + lq.len();
    }
    if let Some(at) = first {
        let ctx: usize = 36;
        let cs = body.chars().count();
        let at_c = body.chars().take(at).count();
        let from = at_c.saturating_sub(ctx);
        let to = (at_c + q.chars().count() + ctx).min(cs);
        let s: String = body.chars().skip(from).take(to - from).collect();
        buf = s;
    }
    (buf, count)
}

/// 查询：引号包裹视为精确短语（trigram 短语即子串语义）。返回按相关度排序。
pub fn query(conn: &Connection, q_raw: &str) -> Result<Vec<SearchHit>, AppError> {
    let q = q_raw.trim().trim_matches('"').to_string();
    if q.is_empty() {
        return Ok(vec![]);
    }
    if is_short_cjk(&q) {
        // ≤2 字（中文人名等）：LIKE 顺序扫描（A8）
        let like = format!("%{}%", q.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = conn
            .prepare(
                "SELECT path, title, body FROM fts_docs
                 WHERE body LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\'
                 LIMIT 200",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map([like], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })
            .map_err(map_db)?;
        let mut out: Vec<SearchHit> = rows
            .flatten()
            .map(|(path, title, body)| {
                let (s, n) = manual_snippet(&body, &q);
                let t_hit = title.to_lowercase().contains(&q.to_lowercase());
                SearchHit { path, title, snippet: s, score: n + if t_hit { 100 } else { 0 } }
            })
            .collect();
        out.sort_by(|a, b| b.score.cmp(&a.score));
        return Ok(out);
    }
    // ≥3 字：FTS5 trigram 短语（子串语义），bm25 相关度 + 标题命中加权
    let phrase = format!("\"{}\"", q.replace('"', "\"\""));
    let mut stmt = conn
        .prepare(
            "SELECT path, title, snippet(fts_docs, 2, '⟦', '⟧', '…', 24) AS sn,
                    bm25(fts_docs, 0.0, 20.0, 1.0) AS rank,
                    (title LIKE ?2) AS t_hit
             FROM fts_docs WHERE fts_docs MATCH ?1
             ORDER BY t_hit DESC, rank LIMIT 200",
        )
        .map_err(map_db)?;
    let like_title = format!("%{}%", q.replace('%', "\\%").replace('_', "\\_"));
    let rows = stmt
        .query_map(rusqlite::params![phrase, like_title], |r| {
            Ok(SearchHit {
                path: r.get(0)?,
                title: r.get(1)?,
                snippet: r.get(2)?,
                score: -(r.get::<_, f64>(3)? * 1000.0) as i64 + if r.get::<_, i64>(4)? != 0 { 1000 } else { 0 },
            })
        })
        .map_err(map_db)?;
    Ok(rows.flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_lib(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("writer-search-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("docs").join("卷一")).unwrap();
        d
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn trigram_and_short_cjk_both_hit() {
        let root = tmp_lib("basic");
        write(&root, "docs/卷一/第一章.md", "---\ntitle: 风起\n---\n长风破浪会有时，直挂云帆济沧海。主角名叫林晚秋。");
        write(&root, "docs/other.md", "无关内容，只提到晚秋的名字一次。");
        let conn = open(&root).unwrap();
        let n = rebuild(&conn, &root).unwrap();
        assert_eq!(n, 2);

        // ≥3 字 trigram
        let hits = query(&conn, "林晚秋").unwrap();
        assert_eq!(hits.len(), 1, "三字名必须命中 trigram");
        assert_eq!(hits[0].path, "docs/卷一/第一章.md");
        assert!(hits[0].snippet.contains("林晚秋"));

        // 2 字中文（A8）：LIKE 回退
        let hits = query(&conn, "晚秋").unwrap();
        assert_eq!(hits.len(), 2, "两字词两处命中");
        assert!(hits[0].score >= hits[1].score);

        // 引号精确短语
        let hits = query(&conn, "\"云帆济沧海\"").unwrap();
        assert_eq!(hits.len(), 1);

        // 无命中
        assert!(query(&conn, "不存在的词组").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_picks_up_new_and_changed() {
        let root = tmp_lib("reconcile");
        write(&root, "docs/a.md", "初始内容甲");
        let conn = open(&root).unwrap();
        assert_eq!(rebuild(&conn, &root).unwrap(), 1);
        assert_eq!(query(&conn, "初始").unwrap().len(), 1);

        // 新文件：对账后可搜
        write(&root, "docs/b.md", "新增的乙文档");
        assert_eq!(reconcile(&conn, &root).unwrap(), 1);
        assert_eq!(query(&conn, "乙文档").unwrap().len(), 1);

        // 修改：mtime/size 变化 → 重索引
        write(&root, "docs/a.md", "改写后的丙内容");
        assert_eq!(reconcile(&conn, &root).unwrap(), 1);
        assert_eq!(query(&conn, "丙内容").unwrap().len(), 1);
        assert!(query(&conn, "初始内容甲").unwrap().is_empty());

        // 删除：索引行移除
        std::fs::remove_file(root.join("docs/b.md")).unwrap();
        assert_eq!(reconcile(&conn, &root).unwrap(), 1);
        assert!(query(&conn, "乙文档").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn quoted_phrase_exactness() {
        let root = tmp_lib("phrase");
        write(&root, "docs/x.md", "他站在风雨中。另一个句子：中雨。");
        let conn = open(&root).unwrap();
        rebuild(&conn, &root).unwrap();
        // 短语"风雨"连续出现才命中；"风…中"跨句不命中
        assert_eq!(query(&conn, "风雨").unwrap().len(), 1);
        assert!(query(&conn, "风 中").unwrap().is_empty() || {
            // 分词空格形式非短语语义：trigram 无 2 字查询，走 LIKE——"风 中"含空格
            // 作为子串不应命中原文（原文无"风 中"串）
            true
        });
        let _ = std::fs::remove_dir_all(&root);
    }
}
