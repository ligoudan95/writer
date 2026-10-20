//! meta.db（SQLite，规格 6.2）：AI Provider / 对话 / 请求日志的本地属主。
//! 对话历史**仅存本地、不同步**（FR-AI-04 隐私设计）；重建时可清除。

use std::path::Path;

use rusqlite::Connection;

use crate::library::AppError;

pub struct MetaDb {
    conn: Connection,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProvider {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// 形如 https://host[:port]；完整端点 = {base}/v1/chat/completions（OpenAI 兼容，FR-AI-01）
    pub base_url: String,
    pub model: String,
    pub enabled: bool,
    /// 流式开关
    pub stream: bool,
    /// 是否默认 Provider（存 kv，列表时合成）
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConversation {
    pub id: String,
    pub doc_uuid: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiMessage {
    pub id: i64,
    pub conv_id: String,
    pub role: String,
    pub content: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiRequestLog {
    pub time: u64,
    pub provider: String,
    pub chars: u64,
    pub kind: String,
}

impl MetaDb {
    pub fn open(root: &Path) -> Result<Self, AppError> {
        let dir = root.join(".writer");
        std::fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("meta.db"))
            .map_err(|e| AppError::new("db", e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS ai_providers(
                id TEXT PRIMARY KEY, name TEXT NOT NULL, base_url TEXT NOT NULL,
                model TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
                stream INTEGER NOT NULL DEFAULT 1);
             CREATE TABLE IF NOT EXISTS kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS ai_conversations(
                id TEXT PRIMARY KEY, doc_uuid TEXT NOT NULL, title TEXT NOT NULL,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_conv_doc ON ai_conversations(doc_uuid);
             CREATE TABLE IF NOT EXISTS ai_messages(
                id INTEGER PRIMARY KEY AUTOINCREMENT, conv_id TEXT NOT NULL,
                role TEXT NOT NULL, content TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_msg_conv ON ai_messages(conv_id);
             CREATE TABLE IF NOT EXISTS ai_request_log(
                id INTEGER PRIMARY KEY AUTOINCREMENT, time INTEGER NOT NULL,
                provider TEXT NOT NULL, chars INTEGER NOT NULL, kind TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS prompt_templates(
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                category TEXT NOT NULL, content TEXT NOT NULL, is_builtin INTEGER NOT NULL DEFAULT 0);",
        )
        .map_err(|e| AppError::new("db", e.to_string()))?;
        Ok(Self { conn })
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    // -- Provider ------------------------------------------------------------

    pub fn provider_list(&self) -> Vec<AiProvider> {
        let default_id = self.kv_get("default_provider").unwrap_or_default();
        let mut stmt = match self.conn.prepare(
            "SELECT id, name, base_url, model, enabled, stream FROM ai_providers ORDER BY rowid",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| {
            Ok(AiProvider {
                id: r.get(0)?,
                name: r.get(1)?,
                base_url: r.get(2)?,
                model: r.get(3)?,
                enabled: r.get::<_, i64>(4)? != 0,
                stream: r.get::<_, i64>(5)? != 0,
                is_default: false,
            })
        });
        match rows {
            Ok(it) => it
                .flatten()
                .map(|mut p| {
                    p.is_default = !default_id.is_empty() && p.id == default_id;
                    p
                })
                .collect(),
            Err(_) => vec![],
        }
    }

    pub fn provider_get(&self, id: &str) -> Option<AiProvider> {
        self.provider_list().into_iter().find(|p| p.id == id)
    }

    pub fn provider_upsert(&self, p: &AiProvider) -> Result<(), AppError> {
        self.conn
            .execute(
                "INSERT INTO ai_providers(id,name,base_url,model,enabled,stream) VALUES(?,?,?,?,?,?)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, base_url=excluded.base_url,
                   model=excluded.model, enabled=excluded.enabled, stream=excluded.stream",
                rusqlite::params![p.id, p.name, p.base_url, p.model, p.enabled as i64, p.stream as i64],
            )
            .map_err(|e| AppError::new("db", e.to_string()))?;
        Ok(())
    }

    pub fn provider_delete(&self, id: &str) {
        let _ = self.conn.execute("DELETE FROM ai_providers WHERE id=?1", [id]);
        if self.kv_get("default_provider").as_deref() == Some(id) {
            self.kv_set("default_provider", "");
        }
    }

    // -- kv -------------------------------------------------------------------

    pub fn kv_get(&self, k: &str) -> Option<String> {
        self.conn
            .query_row("SELECT v FROM kv WHERE k=?1", [k], |r| r.get(0))
            .ok()
    }

    pub fn kv_set(&self, k: &str, v: &str) {
        let _ = self.conn.execute(
            "INSERT INTO kv(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [k, v],
        );
    }

    // -- 对话（FR-AI-04） ------------------------------------------------------

    /// 取文档最近会话，无则建
    pub fn conversation_ensure(&self, doc_uuid: &str, title: &str) -> Result<AiConversation, AppError> {
        if let Some(c) = self.conversation_list(doc_uuid).into_iter().next() {
            return Ok(c);
        }
        let c = AiConversation {
            id: uuid::Uuid::new_v4().to_string(),
            doc_uuid: doc_uuid.to_string(),
            title: title.to_string(),
            created_at: Self::now(),
            updated_at: Self::now(),
        };
        self.conn
            .execute(
                "INSERT INTO ai_conversations(id,doc_uuid,title,created_at,updated_at) VALUES(?,?,?,?,?)",
                rusqlite::params![c.id, c.doc_uuid, c.title, c.created_at, c.updated_at],
            )
            .map_err(|e| AppError::new("db", e.to_string()))?;
        Ok(c)
    }

    pub fn conversation_list(&self, doc_uuid: &str) -> Vec<AiConversation> {
        let mut stmt = match self.conn.prepare(
            "SELECT id,doc_uuid,title,created_at,updated_at FROM ai_conversations
             WHERE doc_uuid=?1 ORDER BY updated_at DESC",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([doc_uuid], |r| {
            Ok(AiConversation {
                id: r.get(0)?,
                doc_uuid: r.get(1)?,
                title: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        });
        match rows {
            Ok(it) => it.flatten().collect(),
            Err(_) => vec![],
        }
    }

    pub fn message_list(&self, conv_id: &str) -> Vec<AiMessage> {
        let mut stmt = match self
            .conn
            .prepare("SELECT id,conv_id,role,content,created_at FROM ai_messages WHERE conv_id=?1 ORDER BY id")
        {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([conv_id], |r| {
            Ok(AiMessage {
                id: r.get(0)?,
                conv_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                created_at: r.get(4)?,
            })
        });
        match rows {
            Ok(it) => it.flatten().collect(),
            Err(_) => vec![],
        }
    }

    pub fn message_append(&self, conv_id: &str, role: &str, content: &str) -> Result<(), AppError> {
        let now = Self::now();
        self.conn
            .execute(
                "INSERT INTO ai_messages(conv_id,role,content,created_at) VALUES(?,?,?,?)",
                rusqlite::params![conv_id, role, content, now],
            )
            .map_err(|e| AppError::new("db", e.to_string()))?;
        let _ = self.conn.execute(
            "UPDATE ai_conversations SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now, conv_id],
        );
        Ok(())
    }

    // -- 请求日志（FR-AI-06：时间/Provider/字数，不含内容） ----------------------

    pub fn log_request(&self, provider: &str, chars: u64, kind: &str) {
        let _ = self.conn.execute(
            "INSERT INTO ai_request_log(time,provider,chars,kind) VALUES(?,?,?,?)",
            rusqlite::params![Self::now(), provider, chars as i64, kind],
        );
    }

    pub fn request_log(&self, limit: usize) -> Vec<AiRequestLog> {
        let mut stmt = match self
            .conn
            .prepare("SELECT time,provider,chars,kind FROM ai_request_log ORDER BY id DESC LIMIT ?")
        {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(AiRequestLog {
                time: r.get(0)?,
                provider: r.get(1)?,
                chars: r.get::<_, i64>(2)? as u64,
                kind: r.get(3)?,
            })
        });
        match rows {
            Ok(it) => it.flatten().collect(),
            Err(_) => vec![],
        }
    }

    pub fn request_log_clear(&self) {
        let _ = self.conn.execute("DELETE FROM ai_request_log", []);
    }
}
