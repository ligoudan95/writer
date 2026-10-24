//! 敏感词/错别字检测（V1.3）：内置广告法极限词表 + 用户自定义词表 +
//! 叠词冗余规则（的的/了了等）。纯文本扫描，结果定位到篇目与上下文。
//! 只提示不改动——处置（改写/忽略）由作者决定。

use crate::frontmatter;
use crate::library::AppError;

#[derive(serde::Serialize, Clone, Debug)]
pub struct LintHit {
    pub word: String,
    /// sensitive | custom | duplication
    pub kind: String,
    pub path: String,
    pub count: usize,
    /// 首个命中上下文
    pub context: String,
}

/// 广告法极限词（内置，发布场景红线词——仅提示，是否适用由作者判断）
const BUILTIN: &[&str] = &[
    "最好", "最佳", "最优", "最强", "最先进", "最优秀", "最高级", "最低价", "最便宜", "最流行",
    "第一", "NO.1", "TOP1", "顶级", "极品", "绝无仅有", "史无前例", "万能", "完美", "永久",
    "绝对", "国家级", "世界级", "宇宙级", "全网第一", "销量第一", "排名第一", "独一无二",
    "首个", "首选", "独家", "首选", "王牌", "冠军", "领导品牌", "驰名商标", "特供", "专供",
    "100%", "百分百", "立即见效", "彻底根治", "药到病除", "包治", "无效退款",
];

/// 叠词冗余（常见笔误）：连续重复的助词/副词
const DUP_RULES: &[&str] = &["的的", "了了", "是是", "在在", "很很", "又又", "就就", "都都", "很 的", "了 的"];

fn custom_words(root: &std::path::Path) -> Vec<String> {
    ensure_kv(root)
        .ok()
        .and_then(|c| {
            c.query_row("SELECT v FROM kv WHERE k = 'lint_custom_words'", [], |r| r.get::<_, String>(0))
                .ok()
        })
        .map(|v| {
            v.split(',')
                .map(|w| w.trim().to_string())
                .filter(|w| w.len() >= 2)
                .collect()
        })
        .unwrap_or_default()
}

/// 打开 meta.db 并确保 kv 表存在（search::open 只建 fts 表；kv 归 MetaDb 管，
/// 此处测试环境无 MetaDb 足迹，幂等补建）
fn ensure_kv(root: &std::path::Path) -> Result<rusqlite::Connection, AppError> {
    let conn = crate::search::open(root)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);")
        .map_err(|e| AppError::new("lint", e.to_string()))?;
    Ok(conn)
}

pub fn set_custom_words(root: &std::path::Path, words: &str) -> Result<(), AppError> {
    let conn = ensure_kv(root)?;
    conn.execute(
        "INSERT OR REPLACE INTO kv(k, v) VALUES ('lint_custom_words', ?1)",
        [words],
    )
    .map_err(|e| AppError::new("lint", e.to_string()))?;
    Ok(())
}

pub fn get_custom_words(root: &std::path::Path) -> String {
    custom_words(root).join(",")
}

fn walk_md(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
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

/// 扫描（scope：当前文档路径或空 = 全库）
pub fn check(root: &std::path::Path, scope: &str) -> Vec<LintHit> {
    let mut files = Vec::new();
    if scope.is_empty() {
        walk_md(&root.join("docs"), "docs", &mut files);
    } else {
        files.push(scope.to_string());
    }
    let customs = custom_words(root);
    let mut out = Vec::new();
    for rel in &files {
        let Ok(bytes) = std::fs::read(root.join(rel)) else { continue };
        let content = String::from_utf8_lossy(&bytes).to_string();
        let (_fm, body) = frontmatter::split(&content);
        // 标题不扫（frontmatter 元数据），只扫正文
        for (word, kind) in BUILTIN
            .iter()
            .map(|w| (*w, "sensitive"))
            .chain(customs.iter().map(|w| (w.as_str(), "custom")))
            .chain(DUP_RULES.iter().map(|w| (*w, "duplication")))
        {
            let mut count = 0usize;
            let mut first: Option<usize> = None;
            let mut start = 0usize;
            while let Some(i) = body[start..].find(word) {
                let at = start + i;
                if first.is_none() {
                    first = Some(at);
                }
                count += 1;
                start = at + word.len();
            }
            if let Some(at) = first {
                let ctx: String = body
                    .chars()
                    .skip(at.saturating_sub(14))
                    .take(word.chars().count() + 28)
                    .collect();
                out.push(LintHit {
                    word: word.to_string(),
                    kind: kind.to_string(),
                    path: rel.clone(),
                    count,
                    context: ctx.replace('\n', " "),
                });
            }
        }
    }
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.path.cmp(&b.path)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_builtin_custom_and_duplication() {
        let root = std::env::temp_dir().join(format!("writer-lint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(
            root.join("docs").join("a.md"),
            "---\ntitle: t\n---\n这是最好的时代，的的笔误出现。还有自定义敏感内容。",
        )
        .unwrap();
        set_custom_words(&root, "敏感内容, 另一个").unwrap();
        assert_eq!(get_custom_words(&root), "敏感内容,另一个");

        let hits = check(&root, "");
        assert!(hits.iter().any(|h| h.word == "最好" && h.kind == "sensitive"));
        assert!(hits.iter().any(|h| h.word == "的的" && h.kind == "duplication"));
        assert!(hits.iter().any(|h| h.word == "敏感内容" && h.kind == "custom" && h.count == 1));
        // 上下文包含命中词
        let w = hits.iter().find(|h| h.word == "最好").unwrap();
        assert!(w.context.contains("最好"));
        // 指定 scope 只扫该篇
        assert_eq!(check(&root, "docs/a.md").len(), hits.len());
        let _ = std::fs::remove_dir_all(&root);
    }
}
