use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::frontmatter::{self, Frontmatter};

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

pub struct Library {
    /// 规范化后的库根路径（canonical，内部操作用）
    pub root: PathBuf,
    /// 展示用路径（非 canonical，前端状态栏显示）
    pub display_root: String,
    pub fresh: bool,
    /// 单写者锁：一切 docs/ 文件与 journal 变更都在此锁内完成（3.3 原则 6 初版）
    pub write_lock: Mutex<()>,
}

#[derive(Debug, Serialize, Clone)]
pub struct AppError {
    pub code: String,
    pub message: String,
}

impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::new("io", e.to_string())
    }
}

pub type CmdResult<T> = Result<T, AppError>;

#[derive(Serialize, Clone)]
pub struct TreeNode {
    /// 相对路径（/ 分隔，作为前端 key）
    pub id: String,
    /// 显示标题：frontmatter 优先，文件名剥离序号前缀兜底
    pub name: String,
    /// "folder" | "doc"
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DocPayload {
    pub path: String,
    pub title: String,
    pub body: String,
    pub hash: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct WriteResult {
    pub path: String,
    pub hash: String,
    pub saved_at: u64,
}

#[derive(Serialize, Clone)]
pub struct StartupInfo {
    pub root: String,
    pub fresh: bool,
    pub pending: Vec<DocPayload>,
}

#[derive(Serialize, Deserialize)]
struct JournalEntry {
    rel: String,
    content: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SnapshotInfo {
    pub id: String,
    pub ts: u64,
    pub source: String,
    pub note: String,
    pub words: usize,
}

#[derive(Serialize, Deserialize)]
struct SnapshotFile {
    info: SnapshotInfo,
    content: String,
}

#[derive(Serialize, Deserialize, Default)]
struct AppConfig {
    #[serde(rename = "lastLibrary")]
    last_library: Option<String>,
}

const WELCOME_BODY: &str = r#"# 欢迎使用 Writer

这是你的第一篇文档。左侧是**文库**（章节树），这里是你写作的地方——直接开始输入即可，自动保存在后台进行（状态栏可见）。

## 三分钟上手

- 左侧栏：新建文档 / 文件夹，点击文档即打开
- 工具栏：标题、加粗、*斜体*、列表、表格……全部可点击
- `Ctrl+S` 立即保存，`Ctrl+N` 新建文档，`Ctrl+1~6` 切换标题级别

## 试一试格式

**加粗**、*斜体*、<u>下划线</u>、~~删除线~~、`行内代码`、[链接](https://example.com)

- 无序列表项一
- 无序列表项二

1. 有序列表项
2. 有序列表项

> 引用：写作是把网状的思考，树状地组织，线性地输出。

- [ ] 任务：写完第一章
- [x] 任务：安装 Writer

| 功能 | 快捷入口 |
| --- | --- |
| 新建文档 | Ctrl+N |
| 保存 | Ctrl+S |
| 标题 | Ctrl+1~6 |

```text
代码块也支持（指定语言即可高亮）
```

祝写作愉快。
"#;

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hash_str(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// 快照目录键：稳定且文件系统安全
fn doc_key(rel: &str) -> String {
    hash_str(rel.as_bytes())[..16].to_string()
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x3040..=0x30FF)
}

/// 字数（FR-EDT-07 口径：CJK 逐字计，西文/数字连续串计 1）
fn count_words(s: &str) -> usize {
    let mut n = 0usize;
    let mut in_word = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if !in_word {
                n += 1;
                in_word = true;
            }
        } else {
            in_word = false;
            if is_cjk(c) {
                n += 1;
            }
        }
    }
    n
}

/// 原子写：临时文件 + rename（Windows 下 MoveFileEx REPLACE_EXISTING，原子替换）
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let uniq = &hash_str(bytes)[..8];
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{uniq}"));
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// 文件名清洗：非法字符→空格，去首尾点与空白，空则"无标题"
pub fn sanitize_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\t' | '\r' | '\n' => ' ',
            _ => c,
        })
        .collect();
    s = s.trim().trim_end_matches('.').trim().to_string();
    while s.starts_with('.') {
        s = s[1..].trim_start().to_string();
    }
    if s.is_empty() {
        s = "无标题".into();
    }
    s.chars().take(60).collect::<String>().trim().to_string()
}

/// 剥离排序前缀 "NN-" / "NNN-"（FR-DOC-02：前缀仅是排序元数据）
pub fn display_name_from_filename(name: &str) -> String {
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let chars: Vec<char> = stem.chars().collect();
    let mut seen_digit = false;
    let mut split_at: Option<usize> = None;
    for (i, c) in chars.iter().enumerate() {
        if c.is_ascii_digit() {
            seen_digit = true;
        } else if *c == '-' && seen_digit && i > 0 {
            split_at = Some(i + 1);
            break;
        } else {
            break;
        }
    }
    match split_at {
        Some(i) => chars[i..].iter().collect::<String>().trim().to_string(),
        None => stem.to_string(),
    }
}

/// 自然排序（数字段按数值比较）
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ab: Vec<char> = a.chars().collect();
    let bb: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    while i < ab.len() && j < bb.len() {
        let (ca, cb) = (ab[i], bb[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let mut na = String::new();
            while i < ab.len() && ab[i].is_ascii_digit() {
                na.push(ab[i]);
                i += 1;
            }
            let mut nb = String::new();
            while j < bb.len() && bb[j].is_ascii_digit() {
                nb.push(bb[j]);
                j += 1;
            }
            let ta = na.trim_start_matches('0');
            let tb = nb.trim_start_matches('0');
            let ord = ta
                .len()
                .cmp(&tb.len())
                .then_with(|| ta.cmp(tb))
                .then_with(|| nb.len().cmp(&na.len()));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            let ord = ca
                .to_lowercase()
                .next()
                .cmp(&cb.to_lowercase().next())
                .then_with(|| ca.cmp(&cb));
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    (ab.len() - i).cmp(&(bb.len() - j))
}

fn app_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Writer").join("config.json"))
}

// ---------------------------------------------------------------------------
// Library 实现
// ---------------------------------------------------------------------------

impl Library {
    /// 启动逻辑：上次文库优先；否则零配置创建默认库（FR-UI-05）
    pub fn startup() -> Result<Library, AppError> {
        let last = app_config_path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<AppConfig>(&s).ok())
            .and_then(|c| c.last_library)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir());

        let lib = match last {
            Some(root) => Library::open(&root, false)?,
            None => {
                let docs = dirs::document_dir()
                    .ok_or_else(|| AppError::new("io", "无法定位用户文档目录"))?;
                let root = docs.join("Writer").join("我的文库");
                let fresh = !root.exists();
                Library::open(&root, fresh)?
            }
        };
        lib.save_last_library();
        Ok(lib)
    }

    fn open(root: &Path, fresh: bool) -> Result<Library, AppError> {
        // 根治加固：库路径必须绝对——相对路径会随 CWD 漂移，
        // 曾导致在 exe 目录下解析出"影子库"路径的瞬态故障
        if !root.is_absolute() {
            return Err(AppError::new(
                "forbidden",
                format!("库路径必须为绝对路径：{}", root.display()),
            ));
        }
        fs::create_dir_all(root.join("docs"))?;
        for sub in ["journal", "trash", "snapshots", "backups", "logs"] {
            fs::create_dir_all(root.join(".writer").join(sub))?;
        }
        if fresh {
            let lib = Library {
                root: root.canonicalize()?,
                display_root: root.to_string_lossy().to_string(),
                fresh,
                write_lock: Mutex::new(()),
            };
            lib.write_welcome()?;
            Ok(lib)
        } else {
            Ok(Library {
                root: root.canonicalize()?,
                display_root: root.to_string_lossy().to_string(),
                fresh,
                write_lock: Mutex::new(()),
            })
        }
    }

    fn write_welcome(&self) -> Result<(), AppError> {
        let path = self.root.join("docs").join("欢迎使用.md");
        if path.exists() {
            return Ok(());
        }
        let mut fm = Frontmatter::default();
        fm.set("id", &uuid::Uuid::new_v4().to_string());
        fm.set("title", "欢迎使用");
        fm.set("created", &now_secs().to_string());
        fm.set("fmt", "1");
        let full = format!("{}\n{}", fm.render(), WELCOME_BODY);
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        atomic_write(&path, full.as_bytes())?;
        Ok(())
    }

    fn save_last_library(&self) {
        if let Some(p) = app_config_path() {
            if let Some(dir) = p.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let cfg = AppConfig {
                last_library: Some(self.display_root.clone()),
            };
            if let Ok(json) = serde_json::to_string_pretty(&cfg) {
                let _ = fs::write(&p, json);
            }
        }
    }

    /// 相对路径 → 绝对路径，含越界防护（NFR-16 精神：命令层不做泛型 fs）
    fn resolve(&self, rel: &str) -> Result<PathBuf, AppError> {
        let rel = rel.trim().trim_start_matches(['/', '\\']);
        let p = self.root.join(rel);
        let canon = p.canonicalize().map_err(|e| {
            AppError::new("not_found", format!("路径不存在：{rel}（{}）[{e}]", p.display()))
        })?;
        if canon.starts_with(&self.root) {
            Ok(canon)
        } else {
            Err(AppError::new("forbidden", "路径越界"))
        }
    }

    /// 绝对路径 → 相对路径（/ 分隔）
    fn rel_of(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn journal_path(&self, rel: &str) -> PathBuf {
        let slug = &hash_str(rel.as_bytes())[..16];
        self.root
            .join(".writer")
            .join("journal")
            .join(format!("{slug}.json"))
    }

    // -- 文档树 ------------------------------------------------------------

    /// 树 id 统一为**库相对路径**（含 docs/ 前缀），与读写/同步命令的路径语义一致
    pub fn list(&self) -> Vec<TreeNode> {
        walk_dir(&self.root.join("docs"), "docs")
    }

    // -- 文档读写 ----------------------------------------------------------

    pub fn read_doc(&self, rel: &str) -> CmdResult<DocPayload> {
        let path = self.resolve(rel)?;
        let bytes = fs::read(&path)?;
        let hash = hash_str(&bytes);
        let content = String::from_utf8_lossy(&bytes).to_string();
        let (fm, body) = frontmatter::split(&content);
        let title = fm
            .as_ref()
            .and_then(|f| f.get("title"))
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                display_name_from_filename(&path.file_name().unwrap_or_default().to_string_lossy())
            });
        Ok(DocPayload {
            path: rel.to_string(),
            title,
            body,
            hash,
        })
    }

    pub fn write_doc(
        &self,
        app: &tauri::AppHandle,
        rel: &str,
        body: &str,
        expected_hash: &str,
        force: bool,
    ) -> CmdResult<WriteResult> {
        let r = self.write_doc_inner(rel, body, expected_hash, force)?;
        let _ = app.emit(
            "doc:saved",
            serde_json::json!({ "path": rel, "hash": r.hash, "savedAt": r.saved_at }),
        );
        Ok(r)
    }

    /// 可测试内核：写入全流程（护栏 → journal → 原子写 → 清理 → 自动快照）
    pub fn write_doc_inner(
        &self,
        rel: &str,
        body: &str,
        expected_hash: &str,
        force: bool,
    ) -> CmdResult<WriteResult> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.write_doc_locked(rel, body, expected_hash, force)
    }

    /// 锁内内核：调用方必须已持有 write_lock（非重入锁，防死锁）
    fn write_doc_locked(
        &self,
        rel: &str,
        body: &str,
        expected_hash: &str,
        force: bool,
    ) -> CmdResult<WriteResult> {
        let path = self.resolve(rel)?;

        let bytes = fs::read(&path).unwrap_or_default();
        let cur_hash = hash_str(&bytes);
        if !force && cur_hash != expected_hash {
            return Err(AppError::new(
                "conflict",
                "文件已在磁盘上被修改（last-read hash 不匹配），已阻止覆盖",
            ));
        }

        // 保留原 frontmatter（缺失则补最小集），未知键原样保留
        let old = String::from_utf8_lossy(&bytes).to_string();
        let (mut fm_opt, _) = frontmatter::split(&old);
        if fm_opt.is_none() {
            let mut f = Frontmatter::default();
            f.set("id", &uuid::Uuid::new_v4().to_string());
            f.set("created", &now_secs().to_string());
            f.set("fmt", "1");
            fm_opt = Some(f);
        }
        let fm = fm_opt.unwrap();
        let full = format!("{}\n{}", fm.render(), body);

        // 1) journal 先行（崩溃恢复：写正文前落盘恢复点）
        let jpath = self.journal_path(rel);
        let entry = JournalEntry {
            rel: rel.to_string(),
            content: full.clone(),
        };
        let jbytes = serde_json::to_vec(&entry).unwrap_or_default();
        atomic_write(&jpath, &jbytes)?;
        // 2) 原子写正文
        atomic_write(&path, full.as_bytes())?;
        // 3) 清理 journal
        let _ = fs::remove_file(&jpath);
        // 4) 每小时自动快照（FR-SAV-03）
        let _ = self.maybe_auto_snapshot(rel);

        let hash = hash_str(full.as_bytes());
        let saved_at = now_secs();
        Ok(WriteResult {
            path: rel.to_string(),
            hash,
            saved_at,
        })
    }

    /// 崩溃恢复：journal 中与磁盘内容不一致（或磁盘文件缺失）的条目
    pub fn journal_pending(&self) -> Vec<DocPayload> {
        let dir = self.root.join(".writer").join("journal");
        let mut out = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => return out,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(&p) else { continue };
            let Ok(entry) = serde_json::from_slice::<JournalEntry>(&bytes) else {
                continue;
            };
            let cur = fs::read(self.root.join(&entry.rel)).unwrap_or_default();
            if hash_str(&cur) == hash_str(entry.content.as_bytes()) {
                continue; // 已写入磁盘，非待恢复
            }
            let (fm, body) = frontmatter::split(&entry.content);
            let title = fm
                .as_ref()
                .and_then(|f| f.get("title"))
                .unwrap_or(&entry.rel)
                .to_string();
            out.push(DocPayload {
                path: entry.rel.clone(),
                title,
                body,
                hash: hash_str(entry.content.as_bytes()),
            });
        }
        out
    }

    // -- 创建 / 重命名 / 删除 ------------------------------------------------

    /// 导入建档（FR-IMP-01）：以给定内容创建文档，文件名取自导入源（.md 后缀）
    pub fn import_doc(&self, folder_rel: &str, filename: &str, content: &str) -> CmdResult<TreeNode> {
        let dir = if folder_rel.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve(folder_rel)?
        };
        if !dir.is_dir() {
            return Err(AppError::new("not_found", "目标文件夹不存在"));
        }
        let stem = filename.trim_end_matches(".md").trim_end_matches(".txt");
        let base = sanitize_filename(stem);
        let filename = unique_name(&dir, &format!("{base}.md"));
        let path = dir.join(&filename);
        let mut fm = Frontmatter::default();
        fm.set("id", &uuid::Uuid::new_v4().to_string());
        fm.set("title", &base);
        fm.set("created", &now_secs().to_string());
        fm.set("fmt", "1");
        let full = format!("{}\n{}", fm.render(), content);
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        atomic_write(&path, full.as_bytes())?;
        Ok(TreeNode {
            id: self.rel_of(&path),
            name: base,
            kind: "doc".into(),
            children: vec![],
        })
    }

    pub fn create_doc(&self, folder_rel: &str, title: &str) -> CmdResult<TreeNode> {
        let dir = if folder_rel.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve(folder_rel)?
        };
        if !dir.is_dir() {
            return Err(AppError::new("not_found", "目标文件夹不存在"));
        }
        let base = sanitize_filename(title);
        let filename = unique_name(&dir, &format!("{base}.md"));
        let path = dir.join(&filename);
        let mut fm = Frontmatter::default();
        fm.set("id", &uuid::Uuid::new_v4().to_string());
        fm.set("title", &base);
        fm.set("created", &now_secs().to_string());
        fm.set("fmt", "1");
        let full = format!("{}\n", fm.render());
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        atomic_write(&path, full.as_bytes())?;
        Ok(TreeNode {
            id: self.rel_of(&path),
            name: base,
            kind: "doc".into(),
            children: vec![],
        })
    }

    pub fn rename_doc(
        &self,
        app: &tauri::AppHandle,
        rel: &str,
        new_title: &str,
    ) -> CmdResult<WriteResult> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.resolve(rel)?;
        let bytes = fs::read(&path)?;
        let content = String::from_utf8_lossy(&bytes).to_string();
        let (mut fm_opt, body) = frontmatter::split(&content);
        if fm_opt.is_none() {
            let mut f = Frontmatter::default();
            f.set("id", &uuid::Uuid::new_v4().to_string());
            f.set("created", &now_secs().to_string());
            f.set("fmt", "1");
            fm_opt = Some(f);
        }
        let mut fm = fm_opt.unwrap();
        fm.set("title", new_title);

        let new_base = sanitize_filename(new_title);
        let old_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut new_path = path.clone();
        if old_name != format!("{new_base}.md") {
            let parent = path.parent().unwrap_or(&self.root).to_path_buf();
            let filename = unique_name(&parent, &format!("{new_base}.md"));
            new_path = parent.join(filename);
            fs::rename(&path, &new_path)?;
        }
        let full = format!("{}\n{}", fm.render(), body);
        atomic_write(&new_path, full.as_bytes())?;
        let new_rel = self.rel_of(&new_path);
        let hash = hash_str(full.as_bytes());
        let saved_at = now_secs();
        let _ = app.emit(
            "doc:renamed",
            serde_json::json!({ "from": rel, "to": new_rel }),
        );
        // 返回新 hash：前端必须采纳，否则下一轮自动保存会假冲突（rename 重写了文件）
        Ok(WriteResult {
            path: new_rel,
            hash,
            saved_at,
        })
    }

    pub fn create_folder(&self, parent_rel: &str, name: &str) -> CmdResult<TreeNode> {
        let dir = if parent_rel.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve(parent_rel)?
        };
        let base = sanitize_filename(name);
        let name = unique_dir_name(&dir, &base);
        let path = dir.join(&name);
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        fs::create_dir_all(&path)?;
        Ok(TreeNode {
            id: self.rel_of(&path),
            name: base,
            kind: "folder".into(),
            children: vec![],
        })
    }

    pub fn rename_folder(&self, rel: &str, new_name: &str) -> CmdResult<String> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.resolve(rel)?;
        if !path.is_dir() {
            return Err(AppError::new("not_found", "不是文件夹"));
        }
        let base = sanitize_filename(new_name);
        let parent = path.parent().unwrap_or(&self.root).to_path_buf();
        let name = unique_dir_name(&parent, &base);
        let new_path = parent.join(name);
        fs::rename(&path, &new_path)?;
        Ok(self.rel_of(&new_path))
    }

    /// 软删除（文档/文件夹通用）：移入 .writer/trash/<时间戳>-<名字>，返回备份名
    pub fn delete_to_trash(&self, rel: &str) -> CmdResult<String> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.resolve(rel)?;
        let trash = self.root.join(".writer").join("trash");
        let orig_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let backup = format!("{}-{}", now_secs(), orig_name);
        let dest = trash.join(&backup);
        fs::rename(&path, &dest)?;
        Ok(backup)
    }

    pub fn restore_from_trash(&self, trash_name: &str, orig_rel: &str) -> CmdResult<()> {
        if trash_name.contains(['/', '\\']) || trash_name.contains("..") {
            return Err(AppError::new("forbidden", "非法的回收站条目"));
        }
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let src = self.root.join(".writer").join("trash").join(trash_name);
        if !src.exists() {
            return Err(AppError::new("not_found", "回收站条目不存在"));
        }
        let dest = self.root.join(orig_rel.trim_start_matches(['/', '\\']));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&src, &dest)?;
        Ok(())
    }

    // -- 快照 / 版本历史（FR-SAV-03，M2） ------------------------------------

    fn snapshots_dir(&self, rel: &str) -> PathBuf {
        self.root.join(".writer").join("snapshots").join(doc_key(rel))
    }

    fn snapshot_write(&self, rel: &str, source: &str, note: &str, content: &str) -> Option<SnapshotInfo> {
        let dir = self.snapshots_dir(rel);
        fs::create_dir_all(&dir).ok()?;
        let ts = now_secs();
        let info = SnapshotInfo {
            id: ts.to_string(),
            ts,
            source: source.to_string(),
            note: note.to_string(),
            words: count_words(content),
        };
        let file = SnapshotFile {
            info: info.clone(),
            content: content.to_string(),
        };
        let path = dir.join(format!("{ts}-{}.json", source));
        atomic_write(&path, &serde_json::to_vec(&file).unwrap_or_default()).ok()?;
        Some(info)
    }

    fn snapshot_files(&self, rel: &str) -> Vec<SnapshotFile> {
        let dir = self.snapshots_dir(rel);
        let mut out = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => return out,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = fs::read(&p) {
                if let Ok(f) = serde_json::from_slice::<SnapshotFile>(&bytes) {
                    out.push(f);
                }
            }
        }
        out.sort_by(|a, b| b.info.ts.cmp(&a.info.ts)); // 新→旧
        out
    }

    pub fn snapshot_list(&self, rel: &str) -> Vec<SnapshotInfo> {
        self.snapshot_files(rel).into_iter().map(|f| f.info).collect()
    }

    pub fn snapshot_create(&self, rel: &str, source: &str, note: &str) -> CmdResult<SnapshotInfo> {
        let path = self.resolve(rel)?;
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let content = fs::read_to_string(&path)?;
        let info = self
            .snapshot_write(rel, source, note, &content)
            .ok_or_else(|| AppError::new("io", "快照写入失败"))?;
        self.snapshot_prune(rel);
        Ok(info)
    }

    pub fn snapshot_read(&self, rel: &str, ts: u64) -> CmdResult<String> {
        self.snapshot_files(rel)
            .into_iter()
            .find(|f| f.info.ts == ts)
            .map(|f| f.content)
            .ok_or_else(|| AppError::new("not_found", "快照不存在"))
    }

    /// 回滚：先把当前内容自动存一份"回滚前"快照（FR-SAV-03），再以快照覆盖正文
    pub fn snapshot_restore(&self, rel: &str, ts: u64) -> CmdResult<WriteResult> {
        let path = self.resolve(rel)?;
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let target = self
            .snapshot_files(rel)
            .into_iter()
            .find(|f| f.info.ts == ts)
            .ok_or_else(|| AppError::new("not_found", "快照不存在"))?;
        let current = fs::read_to_string(&path).unwrap_or_default();
        if !current.trim().is_empty() {
            self.snapshot_write(rel, "pre_rollback", "回滚前自动备份", &current);
        }
        // 已持锁，走锁内内核（write_lock 非重入）
        self.write_doc_locked(rel, &target.content, "", true)
    }

    /// 每小时且内容有变化时自动快照（写入路径调用，失败静默不阻塞保存）
    fn maybe_auto_snapshot(&self, rel: &str) {
        let files = self.snapshot_files(rel);
        let newest_auto = files.iter().find(|f| f.info.source == "auto");
        let due = match newest_auto {
            Some(f) => now_secs().saturating_sub(f.info.ts) >= 3600,
            None => true,
        };
        if due {
            if let Ok(content) = fs::read_to_string(self.root.join(rel)) {
                let _ = self.snapshot_write(rel, "auto", "", &content);
            }
        }
    }

    /// 保留上限（M2 简化版：每文档最多 200 份，超出删最旧；完整策略见 FR-SAV-03）
    fn snapshot_prune(&self, rel: &str) {
        let dir = self.snapshots_dir(rel);
        let mut names: Vec<(u64, std::path::PathBuf)> = match fs::read_dir(&dir) {
            Ok(rd) => rd
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let ts = p
                        .file_stem()?
                        .to_string_lossy()
                        .split('-')
                        .next()?
                        .parse::<u64>()
                        .ok()?;
                    Some((ts, p))
                })
                .collect(),
            Err(_) => return,
        };
        names.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, p) in names.into_iter().skip(200) {
            let _ = fs::remove_file(p);
        }
    }
}

// ---------------------------------------------------------------------------
// 树构建与命名
// ---------------------------------------------------------------------------

fn unique_name(dir: &Path, want: &str) -> String {
    let stem = want.strip_suffix(".md").unwrap_or(want);
    let mut name = want.to_string();
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{stem}-{n}.md");
        n += 1;
    }
    name
}

fn unique_dir_name(dir: &Path, base: &str) -> String {
    let mut name = base.to_string();
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

/// 递归构建文档树：文件夹在前，各自按文件名自然排序；
/// 文档显示名 = frontmatter title（缺失则文件名剥序号前缀）
fn walk_dir(dir: &Path, prefix: &str) -> Vec<TreeNode> {
    let mut folders: Vec<TreeNode> = Vec::new();
    let mut docs: Vec<(String, String)> = Vec::new(); // (文件名, 显示名)
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return folders,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            let children = walk_dir(&path, &rel);
            folders.push(TreeNode {
                id: rel,
                name,
                kind: "folder".into(),
                children,
            });
        } else if name.to_lowercase().ends_with(".md") {
            let display = read_title(&path).unwrap_or_else(|| display_name_from_filename(&name));
            docs.push((name, display));
        }
    }
    folders.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    docs.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let mut out = folders;
    out.extend(docs.into_iter().map(|(name, display)| TreeNode {
        id: if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        },
        name: display,
        kind: "doc".into(),
        children: vec![],
    }));
    out
}

fn read_title(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let content = String::from_utf8_lossy(&bytes);
    let (fm, _) = frontmatter::split(&content);
    fm.and_then(|f| f.get("title").map(|s| s.to_string()))
}

// ---------------------------------------------------------------------------
// 单元测试（NFR-10 测试分层·单元层；M1 验收 A1/A4 的逻辑基础）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_lib(name: &str) -> Library {
        let dir = std::env::temp_dir().join(format!("writer-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Library::open(&dir, false).expect("打开测试库失败")
    }

    #[test]
    fn frontmatter_roundtrip_preserves_unknown_keys() {
        let md = "---\nid: abc\ncustom: keep\ntitle: T\n---\n\n正文内容";
        let (fm, body) = frontmatter::split(md);
        let fm = fm.expect("应解析出 frontmatter");
        assert_eq!(fm.get("id"), Some("abc"));
        assert_eq!(fm.get("custom"), Some("keep"));
        assert!(body.contains("正文内容"));
        let mut fm2 = fm;
        fm2.set("title", "新标题");
        let rendered = fm2.render();
        assert!(rendered.contains("custom: keep"), "未知键必须保留");
        assert!(rendered.contains("title: 新标题"));
    }

    #[test]
    fn frontmatter_absent_returns_none() {
        let (fm, body) = frontmatter::split("只是正文，没有 frontmatter");
        assert!(fm.is_none());
        assert_eq!(body, "只是正文，没有 frontmatter");
    }

    #[test]
    fn natural_cmp_orders_numbers_numerically() {
        let mut v = vec!["10-第十章.md", "2-第二章.md", "1-第一章.md"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["1-第一章.md", "2-第二章.md", "10-第十章.md"]);
    }

    #[test]
    fn sanitize_replaces_illegal_chars() {
        let s = sanitize_filename("a/b:c*d?e\"f<g>h|i.");
        for bad in ['/', ':', '*', '?', '"', '<', '>', '|'] {
            assert!(!s.contains(bad), "仍含非法字符 {bad}");
        }
        assert!(!s.trim().is_empty());
    }

    #[test]
    fn open_rejects_relative_path() {
        assert!(Library::open(Path::new("relative/path"), false).is_err());
    }

    #[test]
    fn write_read_and_hash_guard() {
        let lib = tmp_lib("guard");
        let node = lib.create_doc("", "护栏测试").unwrap();

        // 陈旧 hash 必须被拦（FR-SYN-09 保存护栏）
        let err = lib.write_doc_inner(&node.id, "新内容", "stale-hash", false).unwrap_err();
        assert_eq!(err.code, "conflict");

        // 正常写入 → 读回 → hash 一致
        let first = lib.read_doc(&node.id).unwrap();
        let r = lib.write_doc_inner(&node.id, "第一版内容", &first.hash, false).unwrap();
        let after = lib.read_doc(&node.id).unwrap();
        assert_eq!(after.hash, r.hash);
        assert!(after.body.contains("第一版内容"));

        // force 覆盖（崩溃恢复路径）
        lib.write_doc_inner(&node.id, "强制恢复的内容", "", true).unwrap();
        assert!(lib.read_doc(&node.id).unwrap().body.contains("强制恢复的内容"));
    }


    #[test]
    fn fresh_open_creates_welcome_and_layout() {
        // FR-DOC-01 零配置建库：fresh 打开 → 欢迎文档 + .writer 标准目录
        let dir = std::env::temp_dir().join(format!("writer-test-fresh-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        let lib = Library::open(&dir, true).unwrap();
        assert!(dir.join("docs").join("欢迎使用.md").is_file(), "fresh 库应生成欢迎文档");
        for sub in ["journal", "trash", "snapshots", "backups", "logs"] {
            assert!(dir.join(".writer").join(sub).is_dir(), "缺 .writer/{sub}");
        }
        assert_eq!(lib.list().len(), 1);
        // 欢迎文档再次 open（非 fresh）不重复创建
        drop(lib);
        let lib2 = Library::open(&dir, false).unwrap();
        assert_eq!(lib2.list().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn journal_pending_and_recovery_flow() {
        let lib = tmp_lib("journal");
        let node = lib.create_doc("", "崩溃测试").unwrap();

        // 伪造崩溃：journal 写入新内容但磁盘未更新（A1 场景的确定性复现）
        let entry = JournalEntry {
            rel: node.id.clone(),
            content: "---\ntitle: 崩溃测试\n---\n\n恢复内容 ABC123".to_string(),
        };
        let jpath = lib.journal_path(&node.id);
        atomic_write(&jpath, &serde_json::to_vec(&entry).unwrap()).unwrap();

        let pending = lib.journal_pending();
        assert_eq!(pending.len(), 1, "应检测到一条待恢复");
        assert!(pending[0].body.contains("恢复内容 ABC123"));

        // force 恢复 → journal 清理
        lib.write_doc_inner(&node.id, &pending[0].body, "", true).unwrap();
        assert!(lib.journal_pending().is_empty(), "恢复后 journal 应清空");
        assert!(lib.read_doc(&node.id).unwrap().body.contains("恢复内容 ABC123"));
    }

    #[test]
    fn trash_delete_and_restore_roundtrip() {
        let lib = tmp_lib("trash");
        let node = lib.create_doc("", "待删除文档").unwrap();
        let backup = lib.delete_to_trash(&node.id).unwrap();
        assert!(lib.read_doc(&node.id).is_err(), "删除后不可读");
        lib.restore_from_trash(&backup, &node.id).unwrap();
        assert!(lib.read_doc(&node.id).is_ok(), "撤销删除后可读");
    }

    #[test]
    fn snapshot_create_list_restore() {
        let lib = tmp_lib("snapshot");
        let node = lib.create_doc("", "快照测试").unwrap();

        // 写入第一版 → 手动快照
        let first = lib.read_doc(&node.id).unwrap();
        lib.write_doc_inner(&node.id, "第一版：开篇", &first.hash, false).unwrap();
        let snap1 = lib.snapshot_create(&node.id, "manual", "初稿").unwrap();

        // 写入第二版（覆盖）
        let second = lib.read_doc(&node.id).unwrap();
        lib.write_doc_inner(&node.id, "第二版：重写", &second.hash, false).unwrap();
        assert!(lib.read_doc(&node.id).unwrap().body.contains("第二版"));

        // 列表按新→旧
        let list = lib.snapshot_list(&node.id);
        assert!(list.iter().any(|s| s.ts == snap1.ts));

        // 预览快照内容
        let content = lib.snapshot_read(&node.id, snap1.ts).unwrap();
        assert!(content.contains("第一版"));

        // 回滚：先备份当前（pre_rollback），再恢复快照
        lib.snapshot_restore(&node.id, snap1.ts).unwrap();
        assert!(lib.read_doc(&node.id).unwrap().body.contains("第一版"));
        let after = lib.snapshot_list(&node.id);
        assert!(
            after.iter().any(|s| s.source == "pre_rollback"),
            "回滚前必须自动存备份"
        );
    }

    #[test]
    fn word_count_cjk_and_latin() {
        assert_eq!(count_words("你好世界"), 4);
        assert_eq!(count_words("hello world 123"), 3);
        assert_eq!(count_words("中文abc混排42"), 6); // 2 中文 + abc + 42 + ... 逐项：中/文 + abc + 混/排 + 42 = 6
        assert_eq!(count_words(""), 0);
    }

    #[test]
    fn import_doc_creates_sanitized_unique() {
        let lib = tmp_lib("import");
        let node = lib.import_doc("", "我的 导入.txt", "第一段\n\n第二段").unwrap();
        assert!(node.id.starts_with("docs/"));
        assert!(node.id.ends_with(".md"), "txt 后缀应转为 md：{}", node.id);
        let payload = lib.read_doc(&node.id).unwrap();
        assert!(payload.body.contains("第一段"));
        assert!(payload.body.contains("第二段"));
        // 重名导入自动去重
        let node2 = lib.import_doc("", "我的 导入.txt", "x").unwrap();
        assert_ne!(node.id, node2.id, "同名导入必须去重");
    }
}
