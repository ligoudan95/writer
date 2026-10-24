//! Prompt 模板（FR-AI-05，V1.2）：内置润色/续写/讨论起手模板 + 用户自建，
//! JSON 导入导出。表 prompt_templates 已随 meta.db 建好。

use std::path::Path;

use rusqlite::Connection;

use crate::library::AppError;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct PromptTemplate {
    pub id: i64,
    pub name: String,
    /// polish | continue | chat
    pub category: String,
    pub content: String,
    pub is_builtin: bool,
}

fn map_db(e: rusqlite::Error) -> AppError {
    AppError::new("prompts", e.to_string())
}

fn open(root: &Path) -> Result<Connection, AppError> {
    let dir = root.join(".writer");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::new("prompts", e.to_string()))?;
    let conn = Connection::open(dir.join("meta.db")).map_err(|e| AppError::new("prompts", e.to_string()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS prompt_templates(
            id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
            category TEXT NOT NULL, content TEXT NOT NULL, is_builtin INTEGER NOT NULL DEFAULT 0);",
    )
    .map_err(map_db)?;
    Ok(conn)
}

/// 内置模板（首次调用时播种；幂等）
const BUILTINS: &[(&str, &str, &str)] = &[
    ("润色", "polish", "请润色以下文字：保持原意与语气，修正错别字与标点，句式更流畅。"),
    ("口语化改写", "polish", "请把以下文字改写为自然流畅的口语化表达，保持原意。"),
    ("书面化改写", "polish", "请把以下文字改写为严谨的书面语，保持原意。"),
    ("续写下一段", "continue", "请顺着光标处的文风与情节续写 1-2 个自然段，不要重复已有内容，不要解释。"),
    ("续写对话", "continue", "请顺着上下文续写一段人物对话，注意各自的语气与称呼，不要旁白解释。"),
    ("梳理本章情节漏洞", "chat", "帮我梳理本章情节漏洞：逻辑不自洽、时间线矛盾、人物动机缺失的地方，逐条列出。"),
    ("人物一致性检查", "chat", "对照上下文检查人物言行是否前后一致：性格、说话习惯、立场，指出矛盾处。"),
    ("开脑洞", "chat", "基于当前情节给我 5 个可能的发展方向，每个一句话，风格差异化。"),
];

fn seed(conn: &Connection) -> Result<(), AppError> {
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompt_templates WHERE is_builtin = 1", [], |r| r.get(0))
        .map_err(map_db)?;
    if n == 0 {
        for (name, cat, content) in BUILTINS {
            conn.execute(
                "INSERT INTO prompt_templates(name, category, content, is_builtin) VALUES (?1, ?2, ?3, 1)",
                [name, cat, content],
            )
            .map_err(map_db)?;
        }
    }
    Ok(())
}

pub fn list(root: &Path) -> Result<Vec<PromptTemplate>, AppError> {
    let conn = open(root)?;
    seed(&conn)?;
    let mut stmt = conn
        .prepare("SELECT id, name, category, content, is_builtin FROM prompt_templates ORDER BY is_builtin DESC, id")
        .map_err(map_db)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PromptTemplate {
                id: r.get(0)?,
                name: r.get(1)?,
                category: r.get(2)?,
                content: r.get(3)?,
                is_builtin: r.get::<_, i64>(4)? != 0,
            })
        })
        .map_err(map_db)?;
    Ok(rows.flatten().collect())
}

pub fn save(root: &Path, t: &PromptTemplate) -> Result<PromptTemplate, AppError> {
    let conn = open(root)?;
    if t.id == 0 {
        conn.execute(
            "INSERT INTO prompt_templates(name, category, content, is_builtin) VALUES (?1, ?2, ?3, 0)",
            [&t.name, &t.category, &t.content],
        )
        .map_err(map_db)?;
        let id = conn.last_insert_rowid();
        Ok(PromptTemplate { id, ..t.clone() })
    } else {
        // 内置模板不可改（可复制为自定义后编辑）
        let builtin: bool = conn
            .query_row("SELECT is_builtin FROM prompt_templates WHERE id = ?1", [t.id], |r| {
                Ok(r.get::<_, i64>(0)? != 0)
            })
            .unwrap_or(false);
        if builtin {
            return Err(AppError::new("prompts", "内置模板不可修改，可复制为自定义模板"));
        }
        conn.execute(
            "UPDATE prompt_templates SET name = ?1, category = ?2, content = ?3 WHERE id = ?4 AND is_builtin = 0",
            [&t.name, &t.category, &t.content, &t.id.to_string()],
        )
        .map_err(map_db)?;
        Ok(t.clone())
    }
}

pub fn delete(root: &Path, id: i64) -> Result<(), AppError> {
    let conn = open(root)?;
    conn.execute("DELETE FROM prompt_templates WHERE id = ?1 AND is_builtin = 0", [id])
        .map_err(map_db)?;
    Ok(())
}

/// 导入（JSON 数组；同名跳过）
pub fn import_json(root: &Path, json: &str) -> Result<usize, AppError> {
    let arr: Vec<PromptTemplate> = serde_json::from_str(json)
        .map_err(|e| AppError::new("prompts", format!("模板 JSON 无效：{e}")))?;
    let conn = open(root)?;
    let mut n = 0;
    for t in &arr {
        if t.name.trim().is_empty() || t.content.trim().is_empty() {
            continue;
        }
        let dup: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM prompt_templates WHERE name = ?1 AND category = ?2",
                [&t.name, &t.category],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if dup {
            continue;
        }
        conn.execute(
            "INSERT INTO prompt_templates(name, category, content, is_builtin) VALUES (?1, ?2, ?3, 0)",
            [&t.name, &t.category, &t.content],
        )
        .map_err(map_db)?;
        n += 1;
    }
    Ok(n)
}

/// 导出（全部模板为 JSON 数组）
pub fn export_json(root: &Path) -> Result<String, AppError> {
    let all = list(root)?;
    serde_json::to_string_pretty(&all).map_err(|e| AppError::new("prompts", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("writer-prompts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn builtin_seed_crud_and_import() {
        let root = tmp();
        let all = list(&root).unwrap();
        assert_eq!(all.len(), BUILTINS.len(), "首查播种内置模板");
        assert!(all.iter().all(|t| t.is_builtin));

        // 新建
        let t = PromptTemplate {
            id: 0,
            name: "我的润色".into(),
            category: "polish".into(),
            content: "按我的风格润色：{文本}".into(),
            is_builtin: false,
        };
        let saved = save(&root, &t).unwrap();
        assert!(saved.id > 0);
        // 内置不可改
        let b0 = list(&root).unwrap().into_iter().find(|x| x.is_builtin).unwrap();
        assert!(save(&root, &b0).is_err());
        // 删除：内置不可删，自定义可删
        assert!(delete(&root, b0.id).is_ok(), "内置删除为 no-op 不报错");
        let cnt: usize = list(&root).unwrap().iter().filter(|x| x.is_builtin).count();
        assert_eq!(cnt, BUILTINS.len());
        delete(&root, saved.id).unwrap();
        assert_eq!(list(&root).unwrap().len(), BUILTINS.len());

        // 导入导出
        save(&root, &t).unwrap();
        let json = export_json(&root).unwrap();
        let n = import_json(&root, &json).unwrap();
        assert_eq!(n, 0, "同名全部跳过（幂等）");
        let n2 = import_json(&root, r#"[{"id":0,"name":"外来","category":"chat","content":"x","is_builtin":false}]"#).unwrap();
        assert_eq!(n2, 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
