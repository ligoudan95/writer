use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
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
    /// 单写者锁：一切 docs/ 文件与 journal 变更都在此锁内完成（3.3 原则 6）。
    /// 锁按规范化库根全局注册——同步批次线程经 open_existing 打开的第二实例
    /// 与 UI 实例必须共享同一把锁，否则"单写者"名存实亡
    pub write_lock: Arc<Mutex<()>>,
    /// 库锁（FR-SET-04）：.writer/.lock 独占句柄。进程存活期间他人无法打开同库；
    /// 进程死亡时 OS 自动关闭句柄 = 自动释放（崩溃残留无需清理）
    pub lock_file: Option<Arc<fs::File>>,
    /// 接管了崩溃残留的锁（提示用）
    pub took_over_stale_lock: bool,
}

/// 库根 → 写锁注册表：同一库的多个 Library 实例共享一把锁
fn global_lib_lock(canonical_root: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
    g.entry(canonical_root.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// 库锁注册表（FR-SET-04）：进程内引用计数；计数归零时关闭句柄释放锁。
/// 同进程的 UI 实例与同步批次实例共享一个句柄（否则自锁）。
/// 注册表是**模块级**共享——acquire/release 必须操作同一 static
/// （函数局部 static 每函数一份，曾导致 release 恒为空操作、锁永不释放）
fn held_library_locks() -> &'static Mutex<HashMap<PathBuf, (Arc<fs::File>, usize)>> {
    static HELD: OnceLock<Mutex<HashMap<PathBuf, (Arc<fs::File>, usize)>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashMap::new()))
}

fn acquire_library_lock(canonical_root: &Path) -> Result<(Option<Arc<fs::File>>, bool), AppError> {
    use std::os::windows::fs::OpenOptionsExt;
    let held = held_library_locks();
    let mut g = held.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((f, n)) = g.get_mut(canonical_root) {
        *n += 1;
        return Ok((Some(f.clone()), false));
    }
    let lock_path = canonical_root.join(".writer").join(".lock");
    // 先读取残留内容（拿不到独占句柄说明有活进程持有）
    let stale = fs::read_to_string(&lock_path).ok();
    // share_mode(0)：独占——他人（含本进程之外的实例）任何打开都会得到共享冲突
    match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(&lock_path)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = write!(f, "{}:{}", std::process::id(), now_secs());
            let arc = Arc::new(f);
            g.insert(canonical_root.to_path_buf(), (arc.clone(), 1));
            let took_over = stale.is_some();
            Ok((Some(arc), took_over))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32) => {
            Err(AppError::new(
                "library_locked",
                "文库正被另一个 Writer 实例使用，请关闭后重试",
            ))
        }
        Err(e) => Err(e.into()),
    }
}

fn release_library_lock(canonical_root: &Path) {
    let held = held_library_locks();
    let mut g = held.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, n)) = g.get_mut(canonical_root) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            g.remove(canonical_root); // 最后一个引用：Arc 释放 → 句柄关闭 → 锁解除
        }
    }
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
    /// 隐私标记（FR-AI-06 可视化：树形锁标）
    #[serde(default)]
    pub privacy: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DocPayload {
    pub path: String,
    pub title: String,
    pub body: String,
    pub hash: String,
    /// 隐私标记（FR-AI-06：frontmatter privacy，随同步多端生效）
    #[serde(default)]
    pub privacy: bool,
    /// 文档 uuid（frontmatter id；AI 会话绑定等场景用）
    #[serde(default)]
    pub uuid: String,
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
    /// FR-SYN-01：库位于第三方同步盘范围内时显著警告（双引擎冲突风险）
    #[serde(default)]
    pub thirdparty_warnings: Vec<String>,
    /// 启动磁盘对账结果（FR-DOC-01：补 id / 重分配重复 id），通知中心呈现
    #[serde(default)]
    pub audit_warnings: Vec<String>,
    /// 接管了崩溃残留的库锁（FR-SET-04：自动接管并提示）
    #[serde(default)]
    pub took_over_stale_lock: bool,
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

/// 回收站条目（FR-DOC-03 面板）
#[derive(Serialize, Clone)]
pub struct TrashEntry {
    pub name: String,
    pub orig_name: String,
    /// 删除时记录的原相对路径（还原目标；空则回库根 docs/）
    #[serde(default)]
    pub orig_rel: String,
    pub is_dir: bool,
    pub deleted_at: u64,
    pub size: u64,
    pub over_30d: bool,
}

/// 解析备份名 → (ts, 原名)。新格式 `{ts}_{原名}` / `{ts}_{n}_{原名}`（`_` 分隔，
/// 与文件名中的 `-` 排序前缀无歧义）；兼容旧格式 `{ts}-{原名}`
fn parse_trash_name(name: &str) -> (u64, String) {
    if let Some((ts, rest)) = name.split_once('_') {
        if let Ok(t) = ts.parse::<u64>() {
            // 可能带 `{n}_` 去重段：仅当剩余仍含 `_` 且中段为纯数字
            if let Some((n, tail)) = rest.split_once('_') {
                if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                    return (t, tail.to_string());
                }
            }
            return (t, rest.to_string());
        }
    }
    // 旧格式（历史条目）
    if let Some((t, rest)) = name.split_once('-') {
        if let Ok(ts) = t.parse::<u64>() {
            return (ts, rest.to_string());
        }
    }
    (0, name.to_string())
}

/// 递归收集 docs 下全部 .md 相对路径（对账用）；深度上限防 junction 环
fn collect_md_files(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    collect_md_files_inner(dir, prefix, out, 0)
}

fn collect_md_files_inner(dir: &Path, prefix: &str, out: &mut Vec<String>, depth: usize) {
    if depth > 32 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let p = e.path();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if p.is_dir() {
            collect_md_files_inner(&p, &rel, out, depth + 1);
        } else if name.to_lowercase().ends_with(".md") {
            out.push(rel);
        }
    }
}

/// FR-DOC-02：Windows 完整路径 ≤ 260 字符（面向其它应用的可用性；内部 \\?\ 路径
/// 虽不受限，但用户在资源管理器/其它编辑器中会打不开）。用展示路径（非 \\?\）度量
fn ensure_display_path_len(display_root: &str, rel: &str) -> Result<(), AppError> {
    let len = display_root.chars().count() + 1 + rel.chars().count();
    if len > 260 {
        return Err(AppError::new(
            "path_too_long",
            format!("目标路径长度 {len} 超过 260 字符上限，请缩短标题或减少层级"),
        ));
    }
    Ok(())
}

fn rel_of_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 剥离 "NN-" 排序前缀，返回（前缀序号, 不带前缀与 .md 的 stem）
fn split_order_prefix(filename: &str) -> (Option<u64>, String) {
    let stem = filename.strip_suffix(".md").unwrap_or(filename);
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
        Some(i) => {
            let num: String = chars[..i - 1].iter().collect();
            let rest: String = chars[i..].iter().collect();
            match num.parse() {
                Ok(n) => (Some(n), rest),
                Err(_) => (None, stem.to_string()),
            }
        }
        None => (None, stem.to_string()),
    }
}

/// 唯一化但排除自身（重命名场景不得命中自身）
fn unique_name_excluding(dir: &Path, want: &str, exclude: &Path) -> (String, u32) {
    let stem = want.strip_suffix(".md").unwrap_or(want);
    let mut name = want.to_string();
    let mut n = 1u32;
    while dir.join(&name).exists() && dir.join(&name) != exclude {
        n += 1;
        name = format!("{stem}-{n}.md");
    }
    (name, n)
}

#[derive(Serialize, Deserialize, Default)]
struct AppConfig {
    #[serde(rename = "lastLibrary", default)]
    last_library: Option<String>,
    /// 最近打开的库（最多 8 个，新的在前）——"打开其他库"菜单数据源（FR-DOC-01）
    #[serde(default)]
    recent: Vec<String>,
}

const WELCOME_BODY: &str = r#"# 欢迎使用 Writer

这是你的第一篇文档。左侧是**文库**（章节树），这里是你写作的地方——直接开始输入即可，自动保存在后台进行（状态栏可见）。

## 三分钟上手

- **写作**：左侧栏新建文档 / 文件夹，点击文档即打开；工具栏全部格式可点击；`Ctrl+1~6` 切换标题，`F1` 查看全部快捷键
- **导出**：`Ctrl+E` 打开导出面板，支持 TXT / Markdown / Word / PDF 四种格式
- **同步**：设置（侧栏底部 ⚙ 或状态栏）→ 同步，配置 WebDAV 网盘（如坚果云）即可多设备同步
- **AI**：设置 → AI 配置 Provider 后，选中文字 `Ctrl+Shift+P` 润色、光标处 `Ctrl+Shift+Enter` 续写、`Ctrl+Shift+A` 讨论

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
    hash_hex(bytes)
}

/// blake3 内容 hash（同步引擎判定权威，FR-SYN-04：内容 hash 为权威，mtime 仅提示）
pub fn hash_hex(bytes: &[u8]) -> String {
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

/// 原子写：临时文件 + fsync + rename（Windows 下 MoveFileEx REPLACE_EXISTING，原子替换）。
/// fsync 保证断电后 rename 的目标不出现"元数据已持久而数据块缺失"（NFR-07）
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let uniq = &hash_str(bytes)[..8];
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{uniq}"));
    let tmp = PathBuf::from(tmp);
    let mut f = fs::File::create(&tmp)?;
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    drop(f);
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

impl Drop for Library {
    fn drop(&mut self) {
        self.lock_file = None; // 释放本实例的句柄引用；计数归零时锁解除
        release_library_lock(&self.root);
    }
}

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

    /// 已存在库的重建入口（同步批次后台线程用）：与 startup 不同，不写应用级配置
    pub fn open_existing(root: &Path) -> Result<Library, AppError> {
        Self::open(root, false)
    }

    /// 记入"最近库"并更新上次库（库切换/新建后调用）
    pub fn remember_recent(&self) {
        self.save_last_library();
    }

    /// 打开指定库（库切换，FR-DOC-01/FR-UI-05 菜单入口）
    pub fn open_at(root: &Path) -> Result<Library, AppError> {
        if !root.join("docs").is_dir() {
            return Err(AppError::new(
                "not_a_library",
                "所选文件夹不是 Writer 文库（缺少 docs/ 子目录）",
            ));
        }
        Self::open(root, false)
    }

    /// 在指定位置新建库（含《欢迎使用》）
    pub fn create_at(root: &Path) -> Result<Library, AppError> {
        if root.join("docs").exists() {
            return Err(AppError::new("exists", "目标位置已存在文库，请直接打开"));
        }
        Self::open(root, true)
    }

    /// 最近打开的库列表（应用级，卸载后重装虽清空，默认库可被自动发现——FR-PKG-03）
    pub fn recent_libraries() -> Vec<String> {
        Self::app_config()
            .map(|c| c.recent)
            .unwrap_or_default()
    }

    fn app_config() -> Option<AppConfig> {
        app_config_path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
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
        let canon = root.canonicalize()?;
        // 库锁（FR-SET-04）：其他实例持有时明确拒绝；崩溃残留自动接管并提示
        let (lock_file, took_over_stale_lock) = acquire_library_lock(&canon)?;
        let lib = Library {
            root: canon.clone(),
            display_root: root.to_string_lossy().to_string(),
            fresh,
            write_lock: global_lib_lock(&canon),
            lock_file,
            took_over_stale_lock,
        };
        if fresh {
            lib.write_welcome()?;
        }
        Ok(lib)
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
            let mut cfg = Self::app_config().unwrap_or_default();
            cfg.last_library = Some(self.display_root.clone());
            cfg.recent.retain(|r| r != &self.display_root); // 先移除旧位置（dedup 只去相邻）
            cfg.recent.insert(0, self.display_root.clone());
            cfg.recent.retain(|r| !r.is_empty());
            cfg.recent.truncate(8);
            if let Ok(json) = serde_json::to_string_pretty(&cfg) {
                let _ = atomic_write(&p, json.as_bytes());
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

    /// 文档域解析：限定 docs/ 之内——文档命令不得触达 .writer/（配置/DB/快照/回收站，
    /// NFR-16 能力最小化：防被污染的前端读写内部区破坏同步状态与恢复数据）
    fn resolve_doc(&self, rel: &str) -> Result<PathBuf, AppError> {
        let p = self.resolve(rel)?;
        let docs_root = self.root.join("docs");
        if p == docs_root || p.starts_with(&docs_root) {
            Ok(p)
        } else {
            Err(AppError::new("forbidden", "仅允许访问文档区（docs/）"))
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
    }    // -- 文档树 ------------------------------------------------------------

    /// 树 id 统一为**库相对路径**（含 docs/ 前缀），与读写/同步命令的路径语义一致
    pub fn list(&self) -> Vec<TreeNode> {
        walk_dir(&self.root.join("docs"), "docs")
    }

    // -- 文档读写 ----------------------------------------------------------

    pub fn read_doc(&self, rel: &str) -> CmdResult<DocPayload> {
        let path = self.resolve_doc(rel)?;
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
        let privacy = fm.as_ref().is_some_and(is_privacy);
        let uuid = fm.as_ref().and_then(|f| f.get("id")).unwrap_or("").to_string();
        Ok(DocPayload {
            path: rel.to_string(),
            title,
            body,
            hash,
            privacy,
            uuid,
        })
    }

    /// 隐私标记开关（FR-AI-06）：写 frontmatter，随同步多端生效；返回新 hash 供前端采纳
    pub fn set_privacy(&self, rel: &str, on: bool) -> CmdResult<WriteResult> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.resolve_doc(rel)?;
        let bytes = fs::read(&path)?;
        let content = String::from_utf8_lossy(&bytes).to_string();
        let (fm_opt, body) = frontmatter::split(&content);
        let mut fm = fm_opt.unwrap_or_default();
        if on {
            fm.set("privacy", "true");
        } else {
            fm.entries.retain(|(k, _)| k != "privacy");
        }
        let full = format!("{}
{}", fm.render(), body);
        atomic_write(&path, full.as_bytes())?;
        Ok(WriteResult {
            path: rel.to_string(),
            hash: hash_str(full.as_bytes()),
            saved_at: now_secs(),
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
        let path = self.resolve_doc(rel)?;

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
            let uuid2 = fm.as_ref().and_then(|f| f.get("id")).unwrap_or("").to_string();
            out.push(DocPayload {
                path: entry.rel.clone(),
                title,
                body,
                hash: hash_str(entry.content.as_bytes()),
                privacy: fm.as_ref().is_some_and(is_privacy),
                uuid: uuid2,
            });
        }
        out
    }

    // -- 创建 / 重命名 / 删除 ------------------------------------------------

    /// 导入建档（FR-IMP-01）：外来 frontmatter 兼容规则——未知键保留；
    /// 外来 id 存在且库内唯一则沿用，缺失/冲突重新分配；正文为剥掉 fm 后的部分（防双 fm）
    pub fn import_doc(&self, folder_rel: &str, filename: &str, content: &str) -> CmdResult<TreeNode> {
        let dir = if folder_rel.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve_doc(folder_rel)?
        };
        if !dir.is_dir() {
            return Err(AppError::new("not_found", "目标文件夹不存在"));
        }
        let stem = filename.trim_end_matches(".md").trim_end_matches(".txt");
        let base = sanitize_filename(stem);
        let (foreign_fm, body) = frontmatter::split(content);
        let mut fm = foreign_fm.unwrap_or_default();
        // 导入总是新文件（无"自身路径"需要排除）；同 id 再导入即视为重复 → 重分配
        let id_ok = fm
            .get("id")
            .map(|id| !id.is_empty() && !self.library_has_uuid(id, ""))
            .unwrap_or(false);
        if !id_ok {
            fm.set("id", &uuid::Uuid::new_v4().to_string());
        }
        if fm.get("title").is_none() {
            fm.set("title", &base);
        }
        if fm.get("created").is_none() {
            fm.set("created", &now_secs().to_string());
        }
        fm.set("fmt", "1");
        let full = format!("{}\n{}", fm.render(), body);
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let (filename, _) = unique_name(&dir, &format!("{base}.md"));
        let path = dir.join(&filename);
        ensure_display_path_len(&self.display_root, &self.rel_of(&path))?;
        atomic_write(&path, full.as_bytes())?;
        Ok(TreeNode {
            id: self.rel_of(&path),
            name: base,
            kind: "doc".into(),
            privacy: false,
            children: vec![],
        })
    }

    /// 库内 uuid 存在性（FR-IMP-01 外来 id 唯一性校验；导入与同步下载共用，全量扫描可接受）。
    /// exclude_rel：跳过的路径（同步下载替换同路径同 uuid 文档时，不得把"自己"当重复）
    pub fn library_has_uuid(&self, id: &str, exclude_rel: &str) -> bool {
        let exclude = if exclude_rel.is_empty() {
            None
        } else {
            Some(self.root.join(exclude_rel.trim_start_matches(['/', '\\'])))
        };
        fn scan(dir: &Path, id: &str, exclude: &Option<PathBuf>) -> bool {
            let Ok(rd) = fs::read_dir(dir) else { return false };
            for e in rd.flatten() {
                let p = e.path();
                if Some(&p) == exclude.as_ref() {
                    continue;
                }
                if p.is_dir() {
                    if scan(&p, id, exclude) {
                        return true;
                    }
                } else if p.extension().is_some_and(|x| x == "md") {
                    if let Ok(c) = fs::read_to_string(&p) {
                        if let (Some(fm), _) = frontmatter::split(&c) {
                            if fm.get("id") == Some(id) {
                                return true;
                            }
                        }
                    }
                }
            }
            false
        }
        scan(&self.root.join("docs"), id, &exclude)
    }

    pub fn create_doc(&self, folder_rel: &str, title: &str) -> CmdResult<TreeNode> {
        let dir = if folder_rel.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve_doc(folder_rel)?
        };
        if !dir.is_dir() {
            return Err(AppError::new("not_found", "目标文件夹不存在"));
        }
        let base = sanitize_filename(title);
        let mut fm = Frontmatter::default();
        fm.set("id", &uuid::Uuid::new_v4().to_string());
        fm.set("created", &now_secs().to_string());
        fm.set("fmt", "1");
        // 命名分配必须在锁内：锁外的存在性检查是 TOCTOU，并发创建同名会静默互覆
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        // 排序文件夹内新建 → 占下一个空档编号（FR-DOC-02：插入利用空档，既有零改名）；
        // 普通文件夹 → 去重序号同时作用于标题（FR-DOC-05 新建命名流）
        let (filename, seq) = if Self::folder_is_ordered(&dir) {
            let num = Self::next_order_num(&dir).unwrap_or(10);
            (unique_name(&dir, &format!("{num}-{base}.md")).0, 1)
        } else {
            unique_name(&dir, &format!("{base}.md"))
        };
        let doc_title = if seq == 1 { base.clone() } else { format!("{base} {seq}") };
        fm.set("title", &doc_title);
        let path = dir.join(&filename);
        let rel = self.rel_of(&path);
        ensure_display_path_len(&self.display_root, &rel)?;
        let full = format!("{}\n", fm.render());
        atomic_write(&path, full.as_bytes())?;
        Ok(TreeNode {
            id: self.rel_of(&path),
            name: doc_title,
            kind: "doc".into(),
            privacy: false,
            children: vec![],
        })
    }

    pub fn rename_doc(
        &self,
        app: &tauri::AppHandle,
        rel: &str,
        new_title: &str,
    ) -> CmdResult<WriteResult> {
        let (new_rel, hash, saved_at, old_rel) = self.rename_doc_inner(rel, new_title)?;
        let _ = app.emit(
            "doc:renamed",
            serde_json::json!({ "from": old_rel, "to": new_rel }),
        );
        Ok(WriteResult {
            path: new_rel,
            hash,
            saved_at,
        })
    }

    /// 可测试内核：改标题（frontmatter）+ 必要时改文件名。返回 (新路径, hash, 时间, 旧路径)
    pub fn rename_doc_inner(&self, rel: &str, new_title: &str) -> CmdResult<(String, String, u64, String)> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.resolve_doc(rel)?;
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
        // frontmatter 注入防护：title 过滤控制字符（换行可注入任意键/截断 fm 块）
        let clean_title: String = new_title
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>();
        fm.set("title", &clean_title);

        let new_base = sanitize_filename(new_title);
        let old_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // 排序文件夹内改标题保留 "N-" 编号前缀（FR-DOC-02：前缀仅排序元数据，
        // 改标题不得把文档踢出排序位）
        let (prefix_num, _) = split_order_prefix(&old_name);
        let file_base = match prefix_num {
            Some(n) if Self::folder_is_ordered(path.parent().unwrap_or(&self.root)) => {
                format!("{n}-{new_base}")
            }
            _ => new_base,
        };
        let mut new_path = path.clone();
        if old_name != format!("{file_base}.md") {
            let parent = path.parent().unwrap_or(&self.root).to_path_buf();
            let (filename, _) = unique_name(&parent, &format!("{file_base}.md"));
            new_path = parent.join(filename);
            ensure_display_path_len(&self.display_root, &self.rel_of(&new_path))?;
            fs::rename(&path, &new_path)?;
            self.migrate_snapshots(rel, &self.rel_of(&new_path));
        }
        let full = format!("{}\n{}", fm.render(), body);
        atomic_write(&new_path, full.as_bytes())?;
        let new_rel = self.rel_of(&new_path);
        let hash = hash_str(full.as_bytes());
        let saved_at = now_secs();
        // 返回新 hash：前端必须采纳，否则下一轮自动保存会假冲突（rename 重写了文件）
        Ok((new_rel, hash, saved_at, rel.to_string()))
    }

    pub fn create_folder(&self, parent_rel: &str, name: &str) -> CmdResult<TreeNode> {
        let dir = if parent_rel.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve_doc(parent_rel)?
        };
        if !dir.is_dir() {
            return Err(AppError::new("not_found", "目标文件夹不存在"));
        }
        let base = sanitize_filename(name);
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let name = unique_dir_name(&dir, &base);
        let path = dir.join(&name);
        fs::create_dir_all(&path)?;
        Ok(TreeNode {
            id: self.rel_of(&path),
            name: base,
            kind: "folder".into(),
            privacy: false,
            children: vec![],
        })
    }

    pub fn rename_folder(&self, rel: &str, new_name: &str) -> CmdResult<String> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.resolve_doc(rel)?;
        if !path.is_dir() {
            return Err(AppError::new("not_found", "不是文件夹"));
        }
        let base = sanitize_filename(new_name);
        let parent = path.parent().unwrap_or(&self.root).to_path_buf();
        // 同名改名 = 仅规范文件名，不得因 unique_dir_name 命中自身而变成"名-2"
        if path.file_name().map(|n| n.to_string_lossy() == base).unwrap_or(false) {
            return Ok(self.rel_of(&path));
        }
        // 收集夹内文档相对路径（快照目录随路径迁移）
        let mut olds = Vec::new();
        collect_md_files(&path, &self.rel_of(&path), &mut olds);
        let name = unique_dir_name(&parent, &base);
        let new_path = parent.join(name);
        fs::rename(&path, &new_path)?;
        let new_rel = self.rel_of(&new_path);
        for old in olds {
            self.migrate_snapshots(&old, &old.replace(&format!("{rel}/"), &format!("{new_rel}/")));
        }
        Ok(new_rel)
    }

    /// 软删除（文档/文件夹通用）：移入 .writer/trash/<时间戳>_<名字>，返回备份名供撤销。
    /// 原路径记入 trash/index.json（还原回原位置，FR-DOC-03 AC）。
    /// 分隔符用 `_`：`-` 会与原文件名里的 "N-" 排序前缀/连字符歧义
    pub fn delete_to_trash(&self, rel: &str) -> CmdResult<String> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.resolve_doc(rel)?;
        let trash = self.root.join(".writer").join("trash");
        let orig_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // 同秒删除两个同名文件不得互覆（Windows rename 带 REPLACE_EXISTING）
        let mut backup = format!("{}_{}", now_secs(), orig_name);
        let mut n = 2u32;
        while trash.join(&backup).exists() {
            backup = format!("{}_{n}_{}", now_secs(), orig_name);
            n += 1;
        }
        let dest = trash.join(&backup);
        fs::rename(&path, &dest)?;
        self.trash_index_upsert(&backup, rel);
        Ok(backup)
    }

    /// trash/index.json：备份名 → 原相对路径
    fn trash_index_path(&self) -> PathBuf {
        self.root.join(".writer").join("trash").join("index.json")
    }

    fn trash_index(&self) -> std::collections::HashMap<String, String> {
        fs::read_to_string(self.trash_index_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn trash_index_upsert(&self, backup: &str, orig_rel: &str) {
        let mut idx = self.trash_index();
        idx.insert(backup.to_string(), orig_rel.to_string());
        let _ = atomic_write(&self.trash_index_path(), serde_json::to_vec(&idx).unwrap_or_default().as_slice());
    }

    fn trash_index_remove(&self, backup: &str) {
        let mut idx = self.trash_index();
        idx.remove(backup);
        let _ = atomic_write(&self.trash_index_path(), serde_json::to_vec(&idx).unwrap_or_default().as_slice());
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
        // 还原目标越界防护（纯路径段校验：目标父目录可能尚未创建，不能依赖 canonicalize）
        let rel_norm = orig_rel.trim_start_matches(['/', '\\']).replace('\\', "/");
        if !rel_norm.starts_with("docs/")
            || rel_norm.split('/').any(|c| c.is_empty() || c == ".." || c == ".")
        {
            return Err(AppError::new("forbidden", "非法的还原路径"));
        }
        let dest = self.root.join(&rel_norm);
        // 原位置已有同名新文件：不得静默覆盖（丢新文件），还原为唯一化副本
        let mut final_dest = dest.clone();
        if dest.exists() {
            let stem = dest
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let ext = dest
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let mut n = 2u32;
            while final_dest.exists() {
                final_dest = dest.with_file_name(format!("{stem}（还原 {n}）{ext}"));
                n += 1;
            }
        }
        if let Some(parent) = final_dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&src, &final_dest)?;
        self.trash_index_remove(trash_name);
        Ok(())
    }

    // -- 回收站面板（FR-DOC-03：浏览 / 还原 / 彻底删除 / 30 天提示） -----------------

    /// 列出回收站条目；orig_rel 来自删除时记录的索引（未知则还原回库根 docs/）；
    /// over_30d 供前端"保留超 30 天，建议清理"提示（不自动删）
    pub fn trash_list(&self) -> Vec<TrashEntry> {
        let trash = self.root.join(".writer").join("trash");
        let index = self.trash_index();
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(&trash) else { return out };
        let now = now_secs();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "index.json" {
                continue;
            }
            // 备份名 `{秒级ts}-{原名}`（或 `{ts}-{n}-{原名}`）：剥出 ts 与原名
            let (ts, orig) = parse_trash_name(&name);
            let meta = e.metadata().ok();
            let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
            out.push(TrashEntry {
                name: name.clone(),
                orig_name: orig,
                orig_rel: index.get(&name).cloned().unwrap_or_default(),
                is_dir,
                deleted_at: ts,
                size: meta.map(|m| if is_dir { 0 } else { m.len() }).unwrap_or(0),
                over_30d: ts > 0 && now.saturating_sub(ts) > 30 * 86_400,
            });
        }
        out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
        out
    }

    /// 彻底删除单个条目（前端二次确认）
    pub fn trash_delete(&self, trash_name: &str) -> CmdResult<()> {
        if trash_name.contains(['/', '\\']) || trash_name.contains("..") || trash_name.starts_with('.') {
            return Err(AppError::new("forbidden", "非法的回收站条目"));
        }
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let p = self.root.join(".writer").join("trash").join(trash_name);
        if !p.starts_with(self.root.join(".writer").join("trash")) {
            return Err(AppError::new("forbidden", "非法的回收站条目"));
        }
        if p.is_dir() {
            fs::remove_dir_all(&p)?;
        } else if p.exists() {
            fs::remove_file(&p)?;
        } else {
            return Err(AppError::new("not_found", "回收站条目不存在"));
        }
        self.trash_index_remove(trash_name);
        Ok(())
    }

    /// 清空回收站（模态确认由前端承担）
    pub fn trash_empty(&self) -> CmdResult<usize> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let trash = self.root.join(".writer").join("trash");
        let mut n = 0;
        if let Ok(rd) = fs::read_dir(&trash) {
            for e in rd.flatten() {
                let p = e.path();
                if e.file_name().to_string_lossy() == "index.json" {
                    continue; // 索引不计入条目数
                }
                let ok = if p.is_dir() {
                    fs::remove_dir_all(&p).is_ok()
                } else {
                    fs::remove_file(&p).is_ok()
                };
                if ok {
                    n += 1;
                }
            }
        }
        let _ = fs::remove_file(self.trash_index_path());
        Ok(n)
    }

    // -- 启动磁盘对账（FR-DOC-01 启动序列：uuid/id 校验） ----------------------------

    /// 扫描 docs/：缺 id → 补分配并回写；重复 id → 保留字典序最小路径者（跨设备确定性，
    /// 防 read_dir 顺序不定导致两端保留不同原件），其余重分配。
    /// 全程持 write_lock（防"切走又切回"时与在跑批次交错写回滚远端内容）；
    /// 返回告警列表（通知中心呈现）；文件经原子写，失败逐个跳过不阻塞启动
    pub fn reconcile_ids(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut seen: std::collections::HashSet<String> = Default::default();
        let mut files = Vec::new();
        collect_md_files(&self.root.join("docs"), "docs", &mut files);
        files.sort(); // 字典序 = 确定性 tie-break
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        for rel in files {
            let Ok(bytes) = fs::read(self.root.join(&rel)) else {
                continue;
            };
            let content = String::from_utf8_lossy(&bytes).to_string();
            let (fm_opt, body) = frontmatter::split(&content);
            let mut fm = match fm_opt {
                Some(f) => f,
                None => Frontmatter::default(),
            };
            let id = fm.get("id").unwrap_or("").to_string();
            let action: Option<&str> = if id.is_empty() {
                Some("已补分配 id")
            } else if !seen.insert(id.clone()) {
                Some("id 与其他文档重复，已重新分配")
            } else {
                None
            };
            if let Some(msg) = action {
                fm.set("id", &uuid::Uuid::new_v4().to_string());
                seen.insert(fm.get("id").unwrap_or("").to_string());
                let full = format!("{}\n{}", fm.render(), body);
                if atomic_write(&self.root.join(&rel), full.as_bytes()).is_ok() {
                    warnings.push(format!("{rel}：{msg}"));
                }
            }
        }
        warnings
    }

    // -- 移动（拖拽到文件夹，FR-DOC-02；同步引擎经状态行差异自动以 MOVE 传播） -------

    /// 移动文档到目标文件夹（保持标题/文件名，重名去重）；返回新相对路径
    pub fn move_doc(&self, src_rel: &str, dest_folder: &str) -> CmdResult<String> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let src = self.resolve_doc(src_rel)?;
        if !src.is_file() {
            return Err(AppError::new("not_found", "只能移动文档"));
        }
        let dest_dir = if dest_folder.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve_doc(dest_folder)?
        };
        if !dest_dir.is_dir() {
            return Err(AppError::new("not_found", "目标文件夹不存在"));
        }
        if src.parent() == Some(dest_dir.as_path()) {
            return Ok(src_rel.to_string()); // 同文件夹 = 无操作
        }
        let filename = src.file_name().unwrap_or_default().to_string_lossy().to_string();
        let (filename, _) = unique_name(&dest_dir, &filename);
        let dest = dest_dir.join(&filename);
        ensure_display_path_len(&self.display_root, &self.rel_of(&dest))?;
        fs::rename(&src, &dest)?;
        self.migrate_snapshots(src_rel, &self.rel_of(&dest)); // 快照历史随文档迁移
        Ok(self.rel_of(&dest))
    }

    /// 移动文件夹（含全部内容）；目标不得是自身子目录
    pub fn move_folder(&self, src_rel: &str, dest_parent: &str) -> CmdResult<String> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let src = self.resolve_doc(src_rel)?;
        if !src.is_dir() {
            return Err(AppError::new("not_found", "只能移动文件夹"));
        }
        let dest_dir = if dest_parent.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve_doc(dest_parent)?
        };
        if src == dest_dir || dest_dir.starts_with(&src) {
            return Err(AppError::new("forbidden", "不能移动到自身内部"));
        }
        let name = src.file_name().unwrap_or_default().to_string_lossy().to_string();
        // 收集夹内文档相对路径（快照目录随路径迁移）
        let mut olds = Vec::new();
        collect_md_files(&src, src_rel, &mut olds);
        let dest = dest_dir.join(unique_dir_name(&dest_dir, &name));
        fs::rename(&src, &dest)?;
        let new_rel = self.rel_of(&dest);
        for old in &olds {
            self.migrate_snapshots(old, &old.replace(&format!("{src_rel}/"), &format!("{new_rel}/")));
        }
        Ok(new_rel)
    }

    /// 文档/文件夹重排序号（拖拽排序，FR-DOC-02 步长 10 体系）。
    /// ordered_paths 为目标顺序（该文件夹内同级文档）。**最小改名**（防改名风暴）：
    /// 1) 既有编号构成目标序的最长递增子序列（LIS）→ 这些项零改名；
    /// 2) 其余项在相邻保留项之间分配编号——空档足够则占空档（既有全部零改名，AC）；
    /// 3) 空档耗尽才从该点起对后缀局部重排（顺序稠密编号，禁止无差别全量改名）
    pub fn reorder_docs(&self, folder: &str, ordered_paths: &[String]) -> CmdResult<Vec<String>> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = if folder.trim().is_empty() {
            self.root.join("docs")
        } else {
            self.resolve_doc(folder)?
        };
        let n = ordered_paths.len();
        // 解析既有编号与 stem，并校验同级
        let mut nums: Vec<Option<u64>> = Vec::with_capacity(n);
        let mut paths: Vec<PathBuf> = Vec::with_capacity(n);
        let mut stems: Vec<String> = Vec::with_capacity(n);
        for rel in ordered_paths {
            let p = self.resolve_doc(rel)?;
            let parent = p.parent().ok_or_else(|| AppError::new("io", "无父目录"))?;
            if parent != dir {
                return Err(AppError::new("forbidden", "排序列表含其它文件夹的文档"));
            }
            let fname = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let (num, stem) = split_order_prefix(&fname);
            nums.push(num);
            stems.push(stem);
            paths.push(p);
        }
        // LIS（按编号值，仅 Some 项）→ keep 标记
        let mut keep = vec![false; n];
        let mut lis_len = vec![1usize; n];
        let mut prev_of = vec![usize::MAX; n];
        for i in 0..n {
            if nums[i].is_none() {
                continue;
            }
            for j in 0..i {
                if nums[j].is_some() && nums[j] < nums[i] && lis_len[j] + 1 > lis_len[i] {
                    lis_len[i] = lis_len[j] + 1;
                    prev_of[i] = j;
                }
            }
        }
        let mut cur = (0..n).filter(|&i| nums[i].is_some()).max_by_key(|&i| lis_len[i]);
        while let Some(i) = cur {
            keep[i] = true;
            cur = if prev_of[i] == usize::MAX { None } else { Some(prev_of[i]) };
        }
        // 分配编号：keep 锚点间分段；空档足够占空档，否则后缀稠密重排
        let mut final_nums: Vec<u64> = vec![0; n];
        let mut dense_from: Option<usize> = None; // 从此起（含）全部顺序编号
        let mut last_anchor: u64 = 0;
        let mut seg: Vec<usize> = Vec::new();
        let mut flush_seg = |seg: &mut Vec<usize>, last_anchor: u64, next_anchor: Option<u64>, dense_from: &mut Option<usize>, final_nums: &mut Vec<u64>| {
            if seg.is_empty() {
                return;
            }
            let m = seg.len() as u64;
            match next_anchor {
                Some(q) if q >= last_anchor && q - last_anchor >= 10 * (m + 1) => {
                    // 空档足够：占空档（既有锚点零改名）
                    for (k, &i) in seg.iter().enumerate() {
                        final_nums[i] = last_anchor + 10 * (k as u64 + 1);
                    }
                }
                _ => {
                    // 空档不足/无下锚点：从本段起稠密顺序编号（局部重排）
                    for (k, &i) in seg.iter().enumerate() {
                        final_nums[i] = last_anchor + 10 * (k as u64 + 1);
                    }
                    *dense_from = Some(seg[0]);
                }
            }
            seg.clear();
        };
        for i in 0..n {
            if dense_from.is_some() {
                break;
            }
            if keep[i] {
                flush_seg(&mut seg, last_anchor, nums[i], &mut dense_from, &mut final_nums);
                final_nums[i] = nums[i].unwrap();
                last_anchor = nums[i].unwrap();
            } else {
                seg.push(i);
            }
        }
        if dense_from.is_none() {
            // 末尾段（无下锚点）：顺序编号
            flush_seg(&mut seg, last_anchor, None, &mut dense_from, &mut final_nums);
        }
        // 稠密模式：dense_from 起全部 last_anchor+10 递增
        if let Some(from) = dense_from {
            let mut v = last_anchor;
            for i in from..n {
                v += 10;
                final_nums[i] = v;
            }
        }
        // 落盘改名
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let want = format!("{}-{}.md", final_nums[i], stems[i]);
            let new_name = if want == paths[i].file_name().unwrap_or_default().to_string_lossy() {
                want // 零改名
            } else {
                unique_name_excluding(&dir, &want, &paths[i]).0
            };
            let dest = dir.join(&new_name);
            if dest != paths[i] {
                ensure_display_path_len(&self.display_root, &self.rel_of(&dest))?;
                fs::rename(&paths[i], &dest)?;
                self.migrate_snapshots(&ordered_paths[i], &self.rel_of(&dest));
            }
            out.push(self.rel_of(&dest));
        }
        Ok(out)
    }

    /// 排序文件夹判定：目录内存在任何 "N-" 前缀文档
    fn folder_is_ordered(dir: &Path) -> bool {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.to_lowercase().ends_with(".md") && split_order_prefix(&name).0.is_some() {
                    return true;
                }
            }
        }
        false
    }

    /// 排序文件夹内下一个空档编号（max+10；空目录返回 None）
    fn next_order_num(dir: &Path) -> Option<u64> {
        let mut max = 0u64;
        let mut any = false;
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let (Some(n), _) = split_order_prefix(&name) {
                    any = true;
                    if n > max {
                        max = n;
                    }
                }
            }
        }
        if any { Some(max + 10) } else { None }
    }

    /// 快照目录随文档身份迁移（快照目录键 = 路径 hash；改名/移动/排序后历史必须跟身）。
    /// FR-SAV-03：移动后版本历史不可失联
    pub fn migrate_snapshots(&self, old_rel: &str, new_rel: &str) {
        if old_rel == new_rel {
            return;
        }
        let old_dir = self.snapshots_dir(old_rel);
        let new_dir = self.snapshots_dir(new_rel);
        if !old_dir.exists() {
            return;
        }
        if new_dir.exists() {
            // 合并（罕见：两条路径各有快照）：逐文件搬移，重名加时间戳前缀
            if let Ok(rd) = fs::read_dir(&old_dir) {
                for e in rd.flatten() {
                    let mut dest = new_dir.join(e.file_name());
                    if dest.exists() {
                        dest = new_dir.join(format!("{}-{}", now_secs(), e.file_name().to_string_lossy()));
                    }
                    let _ = fs::rename(e.path(), dest);
                }
            }
            let _ = fs::remove_dir(&old_dir);
        } else if let Some(parent) = new_dir.parent() {
            let _ = fs::create_dir_all(parent);
            let _ = fs::rename(&old_dir, &new_dir);
        }
    }

    // -- 快照 / 版本历史（FR-SAV-03，M2） ------------------------------------

    fn snapshots_dir(&self, rel: &str) -> PathBuf {
        self.root.join(".writer").join("snapshots").join(doc_key(rel))
    }

    fn snapshot_write(&self, rel: &str, source: &str, note: &str, content: &str) -> Option<SnapshotInfo> {
        let dir = self.snapshots_dir(rel);
        fs::create_dir_all(&dir).ok()?;
        // 同秒多次快照（手动+回滚前）id 不得冲突：ts 递增到未占用值
        let mut ts = now_secs();
        while dir.join(format!("{ts}-{source}.json")).exists() {
            ts += 1;
        }
        // 字数口径 = 正文（与编辑器统计一致），不含 frontmatter
        let words = match frontmatter::split(content) {
            (Some(_), body) => count_words(&body),
            (None, _) => count_words(content),
        };
        let info = SnapshotInfo {
            id: ts.to_string(),
            ts,
            source: source.to_string(),
            note: note.to_string(),
            words,
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
        let path = self.resolve_doc(rel)?;
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
        let path = self.resolve_doc(rel)?;
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
        // 快照是整文件（含 frontmatter）：原样原子覆盖——经 write_doc_locked 会再前置
        // 当前 fm，产生双 frontmatter 块且 title/id 等元数据不随快照回滚
        atomic_write(&path, target.content.as_bytes())?;
        let hash = hash_str(target.content.as_bytes());
        Ok(WriteResult {
            path: rel.to_string(),
            hash,
            saved_at: now_secs(),
        })
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
                self.snapshot_prune(rel); // 自动路径同样修剪，防无上限增长
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

/// 文件名去重：want 已存在时追加 -2、-3…；返回 (文件名, 序号——未冲突为 1)
fn unique_name(dir: &Path, want: &str) -> (String, u32) {
    let stem = want.strip_suffix(".md").unwrap_or(want);
    let mut name = want.to_string();
    let mut n = 1u32;
    while dir.join(&name).exists() {
        n += 1;
        name = format!("{stem}-{n}.md");
    }
    (name, n)
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
/// 深度上限防 junction/符号链接自引用导致的栈溢出（FR-DOC-07 体检前置）
fn walk_dir(dir: &Path, prefix: &str) -> Vec<TreeNode> {
    walk_dir_inner(dir, prefix, 0)
}

fn walk_dir_inner(dir: &Path, prefix: &str, depth: usize) -> Vec<TreeNode> {
    const MAX_DEPTH: usize = 32;
    if depth > MAX_DEPTH {
        return Vec::new();
    }
    let mut folders: Vec<TreeNode> = Vec::new();
    let mut docs: Vec<(String, String, bool)> = Vec::new(); // (文件名, 显示名, 隐私)
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
            let children = walk_dir_inner(&path, &rel, depth + 1);
            folders.push(TreeNode {
                id: rel,
                name,
                kind: "folder".into(),
                privacy: false,
                children,
            });
        } else if name.to_lowercase().ends_with(".md") {
            let fm = read_fm(&path);
            let display = fm
                .as_ref()
                .and_then(|f| f.get("title"))
                .map(|s| s.to_string())
                .unwrap_or_else(|| display_name_from_filename(&name));
            let privacy = fm.as_ref().is_some_and(|f| is_privacy(f));
            docs.push((name, display, privacy));
        }
    }
    folders.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    docs.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let mut out = folders;
    out.extend(docs.into_iter().map(|(name, display, privacy)| TreeNode {
        id: if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        },
        name: display,
        kind: "doc".into(),
        privacy,
        children: vec![],
    }));
    out
}

fn read_fm(path: &Path) -> Option<Frontmatter> {
    let bytes = fs::read(path).ok()?;
    let content = String::from_utf8_lossy(&bytes);
    let (fm, _) = frontmatter::split(&content);
    fm
}

/// privacy 键判定（宽松接受 true/1/yes）
pub fn is_privacy(fm: &Frontmatter) -> bool {
    matches!(
        fm.get("privacy").map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("true") | Some("1") | Some("yes")
    )
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

    /// FR-DOC-05 新建命名流：重名时标题随序号递增（无标题 → 无标题 2），文件名去重 -N
    #[test]
    fn create_doc_title_increments_on_duplicates() {
        let lib = tmp_lib("title-incr");
        let a = lib.create_doc("", "无标题").unwrap();
        let b = lib.create_doc("", "无标题").unwrap();
        let c = lib.create_doc("", "无标题").unwrap();
        assert_eq!(a.name, "无标题");
        assert_eq!(b.name, "无标题 2");
        assert_eq!(c.name, "无标题 3");
        assert!(a.id.ends_with("无标题.md"));
        assert!(b.id.ends_with("无标题-2.md"));
        assert!(c.id.ends_with("无标题-3.md"));
        // frontmatter title 与树显示名一致
        let raw = fs::read_to_string(lib.root.join(&b.id)).unwrap();
        let (fm, _) = frontmatter::split(&raw);
        assert_eq!(fm.unwrap().get("title"), Some("无标题 2"));
    }

    /// 修复回归：正文首行为分隔线 + 普通文字的文档不得被误判为 frontmatter（丢行）
    #[test]
    fn frontmatter_separator_body_not_misjudged() {
        let md = "---\n这不是元数据的一行\n---\n\n正文内容";
        let (fm, body) = frontmatter::split(md);
        assert!(fm.is_none(), "无 key:value 结构 → 不是 frontmatter");
        assert_eq!(body, md, "原文必须完整保留（防静默丢行）");
        // 合法 frontmatter 仍正常解析
        let ok = "---\nid: abc\ntitle: T\n---\n正文";
        let (fm2, body2) = frontmatter::split(ok);
        assert_eq!(fm2.unwrap().get("id"), Some("abc"));
        assert_eq!(body2, "正文");
    }

    /// 修复回归：快照回滚不得产生双 frontmatter，元数据随快照整体回滚
    #[test]
    fn snapshot_restore_no_double_frontmatter() {
        let lib = tmp_lib("snap-restore");
        let node = lib.create_doc("", "回滚测试").unwrap();
        edit_body(&lib, &node.id, "第一版内容");
        let info = lib.snapshot_create(&node.id, "manual", "标注").unwrap();
        edit_body(&lib, &node.id, "第二版内容");
        lib.snapshot_restore(&node.id, info.ts).unwrap();
        let raw = fs::read_to_string(lib.root.join(&node.id)).unwrap();
        assert_eq!(raw.matches("---").count(), 2, "只允许一对 fm 分隔线：{raw}");
        assert!(raw.contains("第一版内容"), "正文回滚到快照版本");
        assert!(!raw.contains("第二版内容"));
        let d = lib.read_doc(&node.id).unwrap();
        assert_eq!(d.body.trim(), "第一版内容");
    }

    /// 修复回归：导入带 frontmatter 的 md 不得产生双 fm；未知键保留、id 唯一性校验
    #[test]
    fn import_preserves_foreign_frontmatter() {
        let lib = tmp_lib("import-fm");
        let content = "---\nid: 11111111-2222-3333-4444-555555555555\ntitle: 外来标题\naliases: [a, b]\n---\n\n外来正文";
        let node = lib.import_doc("", "外来.md", content).unwrap();
        let raw = fs::read_to_string(lib.root.join(&node.id)).unwrap();
        assert_eq!(raw.matches("---").count(), 2, "不得出现双 frontmatter：{raw}");
        assert!(raw.contains("aliases: [a, b]"), "未知键保留");
        assert!(raw.contains("外来正文"));
        // 外来 id 唯一 → 沿用
        assert_eq!(
            lib.read_doc(&node.id).unwrap().uuid,
            "11111111-2222-3333-4444-555555555555"
        );
        // 再导入同一文件：id 冲突 → 重分配
        let node2 = lib.import_doc("", "外来.md", content).unwrap();
        assert_ne!(
            lib.read_doc(&node2.id).unwrap().uuid,
            "11111111-2222-3333-4444-555555555555"
        );
    }

    /// 修复回归：回收站还原路径越界必须拒绝
    #[test]
    fn trash_restore_rejects_escape_paths() {
        let lib = tmp_lib("trash-escape");
        let node = lib.create_doc("", "越界测试").unwrap();
        let backup = lib.delete_to_trash(&node.id).unwrap();
        assert!(
            lib.restore_from_trash(&backup, "docs/../../evil.md").is_err(),
            "../ 逃逸必须拒绝"
        );
        assert!(
            lib.restore_from_trash(&backup, ".writer/config.json").is_err(),
            "还原不得写 .writer 内部区"
        );
        // 合法路径仍可还原
        assert!(lib.restore_from_trash(&backup, &node.id).is_ok());
        assert!(lib.read_doc(&node.id).is_ok());
    }

    /// 修复回归：快照目录随路径迁移（move/reorder/rename 后历史不丢）
    #[test]
    fn snapshots_migrate_on_move_and_reorder() {
        let lib = tmp_lib("snap-move");
        let a = lib.create_doc("", "甲章").unwrap().id;
        let b = lib.create_doc("", "乙章").unwrap().id;
        edit_body(&lib, &a, "甲内容");
        edit_body(&lib, &b, "乙内容");
        let _ = lib.snapshot_create(&a, "manual", "旧位快照").unwrap();
        let _ = lib.snapshot_create(&b, "manual", "乙快照").unwrap();

        // 移动到子文件夹
        lib.create_folder("", "卷一").unwrap();
        let na = lib.move_doc(&a, "docs/卷一").unwrap();
        assert!(lib.snapshot_list(&na).len() >= 1, "移动后快照历史随行");
        assert!(lib.snapshot_list(&a).is_empty(), "旧路径不再持有快照");

        // 重排序号（乙拿到 10- 前缀）
        let nb = &lib.reorder_docs("", &[b.clone()]).unwrap()[0];
        assert!(lib.snapshot_list(nb).len() >= 1, "重排后快照历史随行");
        assert!(nb.ends_with(".md") && !nb.ends_with(".md.md"), "不得出现双 .md：{nb}");
    }

    /// 修复回归：拖拽排序最小改名——既有编号在目标序中递增则零改名（FR-DOC-02 AC）
    #[test]
    fn reorder_minimal_renames() {
        let lib = tmp_lib("reorder-min");
        let a = lib.create_doc("", "一").unwrap().id;
        let b = lib.create_doc("", "二").unwrap().id;
        let c = lib.create_doc("", "三").unwrap().id;
        // 初次排序：10-一 20-二 30-三（返回值为新路径）
        let r1 = lib.reorder_docs("", &[a.clone(), b.clone(), c.clone()]).unwrap();
        let (a, _b, c) = (r1[0].clone(), r1[1].clone(), r1[2].clone());
        assert!(a.ends_with("10-一.md"), "{a}");
        assert!(c.ends_with("30-三.md"), "{c}");
        // 删除"二"空出 20 → 新建"四"拖到中间：四占空档 20，既有文件零重命名（FR-DOC-02 AC）
        lib.delete_to_trash(&_b).unwrap();
        let d = lib.create_doc("", "四").unwrap().id;
        let r2 = lib.reorder_docs("", &[a.clone(), d, c.clone()]).unwrap();
        assert!(a.ends_with("10-一.md"), "既有编号零改名：{a}");
        assert!(r2[1].ends_with("20-四.md"), "新文档占空档编号 20：{}", r2[1]);
        assert!(r2[2].ends_with("30-三.md"), "后续既有零改名：{}", r2[2]);
    }

    /// 修复回归：库锁引用计数——drop 全部实例后注册表清空（release 曾是空操作）
    #[test]
    fn library_lock_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("writer-lock-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        let _ = fs::remove_dir_all(&dir);
        {
            let _lib = Library::open(&dir, false).unwrap();
            // 持有期间注册表有条目
            assert!(held_library_locks()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&dir.canonicalize().unwrap()));
        } // drop → 计数归零 → 条目移除（锁真正释放）
        assert!(
            !held_library_locks()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&dir.canonicalize().unwrap()),
            "库锁必须随最后一个实例 Drop 释放"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 修复回归：对账确定性——重复 id 保留字典序最小路径
    #[test]
    fn reconcile_dedup_deterministic() {
        let lib = tmp_lib("reconcile-det");
        // 两个文件写同一个 id
        for (name, body) in [("B文档", "b"), ("A文档", "a")] {
            let node = lib.create_doc("", name).unwrap();
            let d = lib.read_doc(&node.id).unwrap();
            let raw = fs::read_to_string(lib.root.join(&node.id)).unwrap();
            let (fm, _) = frontmatter::split(&raw);
            let mut f2 = fm.unwrap();
            f2.set("id", "11111111-2222-3333-4444-000000000000");
            let full = format!("{}\n{}", f2.render(), body);
            let _ = d;
            atomic_write(&lib.root.join(&node.id), full.as_bytes()).unwrap();
        }
        let warns = lib.reconcile_ids();
        assert_eq!(warns.len(), 1, "仅一个文件被重分配：{warns:?}");
        // 字典序最小（docs/A文档）保留原 id
        let a_raw = fs::read_to_string(lib.root.join("docs/A文档.md")).unwrap();
        assert!(a_raw.contains("11111111-2222-3333-4444-000000000000"), "A 保留原 id");
        let b_raw = fs::read_to_string(lib.root.join("docs/B文档.md")).unwrap();
        assert!(!b_raw.contains("11111111-2222-3333-4444-000000000000"), "B 被重分配");
    }

    fn edit_body(lib: &Library, rel: &str, body: &str) {
        let d = lib.read_doc(rel).unwrap();
        lib.write_doc_inner(rel, body, &d.hash, false).unwrap();
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
