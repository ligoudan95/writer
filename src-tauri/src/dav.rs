//! WebDAV 存储后端（5.1 StorageBackend 契约）+ NetGateway 执行层。
//! 后端只做无状态传输；身份识别（uuid）、删除/改名判定、冲突决策由 sync.rs 引擎统一完成。
//! 可测试性（5.1）：MockDav 内存实现供 A3/A4/A11–A14 双机模拟回归，脱离真实网络。

use std::time::Duration;

use crate::library::AppError;

// ---------------------------------------------------------------------------
// 契约类型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteEntry {
    /// 相对远端根的路径（/ 分隔，URL 已解码）
    pub path: String,
    pub is_dir: bool,
    /// 服务端 etag；可为空（引擎回退为内容 hash 比对，FR-SYN-04）
    pub etag: String,
    pub size: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
}

/// WebDAV 连接配置（密码经 Secrets 服务按 key 读取，不在此结构中传递）
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DavConfig {
    /// 服务器地址，如 https://dav.jianguoyun.com/dav/
    pub url: String,
    pub username: String,
    /// 远程根目录（相对 url 的路径，/ 分隔）
    pub remote_root: String,
}

// ---------------------------------------------------------------------------
// StorageBackend 契约（5.1）：sync 引擎唯一依赖的后端接口
// ---------------------------------------------------------------------------

pub trait StorageBackend {
    fn test_connection(&mut self) -> Result<TestResult, AppError>;
    /// 递归列出远端根下全部条目（不含根自身；目录条目包含在内）
    fn list_all(&mut self) -> Result<Vec<RemoteEntry>, AppError>;
    fn get(&mut self, path: &str) -> Result<Vec<u8>, AppError>;
    /// 返回 etag（服务端未提供时为空串）
    fn put(&mut self, path: &str, data: &[u8]) -> Result<String, AppError>;
    fn mv(&mut self, from: &str, to: &str) -> Result<(), AppError>;
    fn delete(&mut self, path: &str) -> Result<(), AppError>;
    fn ensure_dir(&mut self, path: &str) -> Result<(), AppError>;
    /// FR-SYN-06 同步加密钩子（V1 预留：恒等变换，V2 启用）
    #[allow(dead_code)]
    fn transform(&self, data: &[u8]) -> Vec<u8> {
        data.to_vec()
    }
}

// ---------------------------------------------------------------------------
// 真实 WebDAV 实现（reqwest blocking；NetGateway 唯一网络出口原则的执行点之一）
// ---------------------------------------------------------------------------

pub struct DavBackend {
    client: reqwest::blocking::Client,
    cfg: DavConfig,
    password: String,
    base: String,
    /// 批内已确认存在的目录前缀（MKCOL 结果缓存；坚果云等按请求频率配额，
    /// 逐次重复 MKCOL 根链会白白消耗配额）
    ensured_dirs: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl DavBackend {
    pub fn new(cfg: DavConfig, password: String) -> Result<Self, AppError> {
        let url = cfg.url.trim().trim_end_matches('/').to_string();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(AppError::new("sync_config", "服务器地址必须以 http(s):// 开头"));
        }
        let client = crate::netcfg::apply_blocking(
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .connect_timeout(Duration::from_secs(15)),
            &crate::netcfg::load(),
        )
        .build()
        .map_err(|e| AppError::new("net", e.to_string()))?;
        Ok(Self {
            base: {
                let root = cfg.remote_root.trim_matches('/');
                if root.is_empty() {
                    url.clone() // 同步到账户根：不带尾斜杠，避免 url_of 拼出 "//"
                } else {
                    format!("{url}/{root}")
                }
            },
            cfg,
            password,
            client,
            ensured_dirs: std::cell::RefCell::new(std::collections::HashSet::new()),
        })
    }

    fn url_of(&self, path: &str) -> String {
        format!("{}/{}", self.base, encode_path(path.trim_start_matches('/')))
    }

    fn auth_header(&self) -> String {
        format!("Basic {}", b64(format!("{}:{}", self.cfg.username, self.password).as_bytes()))
    }

    fn request(&self, method: &str, url: &str, depth: Option<&str>, extra: &[(&str, String)], body: Option<Vec<u8>>) -> Result<reqwest::blocking::Response, AppError> {
        let m = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| AppError::new("net", e.to_string()))?;
        let mut rb = self.client.request(m, url).header("Authorization", self.auth_header());
        if let Some(d) = depth {
            rb = rb.header("Depth", d);
        }
        for (k, v) in extra {
            rb = rb.header(*k, v);
        }
        if let Some(b) = body {
            rb = rb.body(b);
        }
        let resp = rb.send().map_err(map_net_err)?;
        match resp.status().as_u16() {
            200..=207 => Ok(resp),
            401 | 403 => Err(AppError::new("sync_auth", "账号或应用密码错误（401/403）")),
            404 => Err(AppError::new("not_found", "远端路径不存在")),
            409 => Err(AppError::new("net_conflict", "远端状态冲突（409）")),
            429 | 507 => Err(AppError::new("sync_rate_limited", "服务端限流或流量超限（429/507）")),
            412 => Err(AppError::new("sync_precondition", "前置条件失败（412，可能被并发修改）")),
            code => Err(AppError::new(
                "net",
                format!("服务端返回 {code} {}", resp.status().canonical_reason().unwrap_or("")),
            )),
        }
    }
}

fn map_net_err(e: reqwest::Error) -> AppError {
    if e.is_timeout() {
        AppError::new("net_timeout", format!("网络超时：{e}"))
    } else if e.is_connect() {
        AppError::new("net_connect", format!("无法连接服务器：{e}"))
    } else {
        AppError::new("net", e.to_string())
    }
}

impl StorageBackend for DavBackend {
    fn test_connection(&mut self) -> Result<TestResult, AppError> {
        // 根路径 PROPFIND Depth:0：连通 + 鉴权一并验证。
        // 远端根尚未创建（新账户）→ 自动建根并如实回报——同步引擎同款行为，
        // 否则测试按钮永远失败而实际同步能成功，误导用户
        match self.request(
            "PROPFIND",
            &self.base.clone(),
            Some("0"),
            &[],
            Some(PROPFIND_BODY.as_bytes().to_vec()),
        ) {
            Ok(_) => Ok(TestResult { ok: true, message: "连接成功".into() }),
            Err(e) if e.code == "not_found" => {
                self.ensure_dir("")?;
                Ok(TestResult { ok: true, message: "连接成功（远端目录不存在，已自动创建）".into() })
            }
            Err(e) => Err(e),
        }
    }

    fn list_all(&mut self) -> Result<Vec<RemoteEntry>, AppError> {
        // 上限防护：恶意服务器无限返回新目录可令队列永不排空（同步线程挂死）
        const MAX_REMOTE_ENTRIES: usize = 50_000;
        let mut out = Vec::new();
        let mut queue = vec![String::new()]; // 远端根
        while let Some(dir) = queue.pop() {
            if out.len() >= MAX_REMOTE_ENTRIES {
                return Err(AppError::new("net", "远端条目数超过上限（疑似异常响应）"));
            }
            let url = if dir.is_empty() { self.base.clone() } else { self.url_of(&dir) };
            let resp = match self.request("PROPFIND", &url, Some("1"), &[], Some(PROPFIND_BODY.as_bytes().to_vec())) {
                Ok(r) => r,
                // 远端根不存在（新账户未建目录）：自动建根后视同空远端——
                // 否则首绑永远失败，用户被迫去网页端手工建目录
                Err(e) if dir.is_empty() && e.code == "not_found" => {
                    self.ensure_dir("")?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let text = resp.text().map_err(|e| AppError::new("net", e.to_string()))?;
            for entry in parse_propfind(&text) {
                // Depth:1 首个 response 是目录自身，跳过
                if entry.path.is_empty() {
                    continue;
                }
                let full = if dir.is_empty() { entry.path.clone() } else { format!("{dir}/{}", entry.path) };
                // 安全闸（P0）：恶意服务器的文件名（../、\、绝对段、控制字符）一旦
                // 进入本地路径拼接即可写出库外任意路径——非法条目直接丢弃
                if !safe_remote_path(&full) {
                    continue;
                }
                if entry.is_dir {
                    queue.push(full.clone());
                }
                out.push(RemoteEntry {
                    path: full,
                    is_dir: entry.is_dir,
                    etag: entry.etag,
                    size: entry.size,
                });
            }
        }
        Ok(out)
    }

    fn get(&mut self, path: &str) -> Result<Vec<u8>, AppError> {
        let resp = self.request("GET", &self.url_of(path), None, &[], None)?;
        resp.bytes().map(|b| b.to_vec()).map_err(|e| AppError::new("net", e.to_string()))
    }

    fn put(&mut self, path: &str, data: &[u8]) -> Result<String, AppError> {
        if let Some(parent) = parent_of(path) {
            self.ensure_dir(&parent)?;
        }
        let resp = self.request("PUT", &self.url_of(path), None, &[], Some(data.to_vec()))?;
        Ok(resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim_matches('"')
            .to_string())
    }

    fn mv(&mut self, from: &str, to: &str) -> Result<(), AppError> {
        self.request(
            "MOVE",
            &self.url_of(from),
            None,
            &[
                ("Destination", self.url_of(to)),
                // Overwrite:F——目标已存在时失败而非覆盖（引擎回退为冲突路径）
                ("Overwrite", "F".to_string()),
            ],
            None,
        )?;
        Ok(())
    }

    fn delete(&mut self, path: &str) -> Result<(), AppError> {
        self.request("DELETE", &self.url_of(path), None, &[], None)?;
        Ok(())
    }

    /// 逐级 MKCOL 建目录链。**自服务器根起算**（含 remote_root 本身，如 writer/）：
    /// 全新账户的远端根不存在时，仅靠根内相对路径建链必然 409——首绑必须能建根。
    /// path 为相对 remote_root 的路径；空串 = 只建根。405 = 已存在视为成功。
    fn ensure_dir(&mut self, path: &str) -> Result<(), AppError> {
        let root = self.cfg.remote_root.trim_matches('/');
        let rel = path.trim_matches('/');
        let full = if rel.is_empty() { root.to_string() } else { format!("{root}/{rel}") };
        let segs: Vec<&str> = full.split('/').filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            return Ok(()); // 无远端根（同步到账户根）：无需建链
        }
        for i in 0..segs.len() {
            let sub = segs[..=i].join("/");
            if self.ensured_dirs.borrow().contains(&sub) {
                continue;
            }
            let url = format!("{}/{}", self.cfg.url.trim_end_matches('/'), encode_path(&sub));
            let r = self
                .client
                .request(
                    reqwest::Method::from_bytes(b"MKCOL").unwrap(),
                    &url,
                )
                .header("Authorization", self.auth_header())
                .send();
            match r {
                Ok(resp) if matches!(resp.status().as_u16(), 200..=207 | 405) => {}
                Ok(resp) => {
                    return Err(AppError::new("net", format!("MKCOL {sub} 失败：{}", resp.status())))
                }
                Err(e) => return Err(map_net_err(e)),
            }
            self.ensured_dirs.borrow_mut().insert(sub);
        }
        Ok(())
    }
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getetag/><d:getcontentlength/></d:prop></d:propfind>"#;

fn parent_of(path: &str) -> Option<String> {
    let p = path.trim_end_matches('/');
    match p.rfind('/') {
        Some(i) if i > 0 => Some(p[..i].to_string()),
        _ => None,
    }
}

/// 极简 percent-encode（保留 / 与安全字符；CJK 等按 UTF-8 百分号编码）
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(*b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// href 百分号解码（UTF-8）
fn decode_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 远端相对路径安全校验：每段必须非空、非 ./..、无反斜杠、无控制字符、长度合法。
/// 防恶意服务器以 PROPFIND 文件名（`..%2F..`、`C:\…` 等）实施路径穿越
pub(crate) fn safe_remote_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 1024 {
        return false;
    }
    path.split('/').all(|comp| {
        !comp.is_empty()
            && comp != "."
            && comp != ".."
            && !comp.contains('\\')
            && comp.len() <= 255
            && !comp.chars().any(|c| c.is_control())
    })
}

/// XML 基本实体解码（etag 等）
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&") // amp 最后：防 &amp;quot; 双解
}

fn hex(b: Option<&u8>) -> Option<u8> {
    match b {
        Some(c @ b'0'..=b'9') => Some(c - b'0'),
        Some(c @ b'a'..=b'f') => Some(c - b'a' + 10),
        Some(c @ b'A'..=b'F') => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 解析 PROPFIND 207 响应，返回**该目录下的直接子条目名**（前缀无关：d:/D:/无前缀）。
/// 逐个 '<' 扫描标签按 local name 匹配，不依赖 XML 库（响应结构稳定、字段有限）。
pub(crate) fn parse_propfind(xml: &str) -> Vec<RemoteEntry> {
    let mut out = Vec::new();
    // 按 local name "response" 切块
    let mut blocks: Vec<&str> = Vec::new();
    let bytes = xml.as_bytes();
    let mut i = 0usize;
    let mut open: Option<usize> = None;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let tag_end = match xml[i..].find('>') {
                Some(rel) => i + rel,
                None => break,
            };
            let inner = &xml[i + 1..tag_end];
            let is_close = inner.starts_with('/');
            let local = inner
                .trim_start_matches('/')
                .split(':')
                .next_back()
                .unwrap_or(inner);
            if local.eq_ignore_ascii_case("response") {
                if is_close {
                    if let Some(start) = open.take() {
                        blocks.push(&xml[start..tag_end + 1]);
                    }
                } else if open.is_none() {
                    open = Some(i);
                }
            }
            i = tag_end + 1;
        } else {
            i += 1;
        }
    }
    for block in blocks {
        let Some(href) = extract_tag(block, "href") else { continue };
        // href 中的 XML 实体必须解码（文件名含 &amp; 等时与真实名保持一致）
        let href = decode_entities(&href);
        // 目录判定仅认 <resourcetype> 开闭标签之间的 <collection>（local name 匹配，
        // 忽略 ns 前缀）：子串匹配会把文件名/etag 恰含 ":collection" 的普通文件误判为目录，
        // 进而被引擎当作"远端缺失"把本地文档移入回收站；自闭合 <resourcetype/> 无第二次出现 → 非目录
        let is_dir = {
            let lower = block.to_lowercase();
            let mut it = lower.match_indices("resourcetype");
            match (it.next(), it.next()) {
                (Some((i, _)), Some((j, _))) => {
                    lower[i + "resourcetype".len()..j].contains("collection")
                }
                _ => false,
            }
        };
        let etag = extract_tag(block, "getetag").unwrap_or_default();
        let size = extract_tag(block, "getcontentlength")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        // href 可能是绝对 URL：仅取路径部分；再取最后一段 = 直接子条目名
        let path = match href.find("://") {
            Some(idx) => {
                let rest = &href[idx + 3..];
                match rest.find('/') {
                    Some(j) => &rest[j..],
                    None => "/",
                }
            }
            None => href.as_str(),
        };
        let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        let name = decode_path(name);
        if name.is_empty() {
            continue;
        }
        out.push(RemoteEntry {
            path: name,
            is_dir,
            etag: decode_entities(etag.trim()).trim_matches('"').to_string(),
            size,
        });
    }
    out
}

/// 提取任意命名空间前缀下 <ns:tag>…</ns:tag> 的文本（大小写不敏感，剥 CDATA）。
/// 大小写折叠用 to_ascii_lowercase：字节长度不变，索引可安全跨界用于原串切片
/// （Unicode to_lowercase 可能变长，如 ß→SS，跨串索引会切在非字符边界上 panic）
fn extract_tag(block: &str, tag: &str) -> Option<String> {
    let lower = block.to_ascii_lowercase();
    let bytes = block.as_bytes();
    let mut i = 0usize;
    while i < lower.len() {
        let Some(lt) = lower[i..].find('<') else { return None };
        let lt = i + lt;
        let Some(gt_rel) = lower[lt..].find('>') else { return None };
        let gt = lt + gt_rel;
        if gt + 1 >= bytes.len() {
            return None;
        }
        let inner = &block[lt + 1..gt];
        let local = inner.split(':').next_back().unwrap_or(inner);
        if local.eq_ignore_ascii_case(tag) {
            // 在其后的文本中找闭合标签（local name 匹配）
            let mut j = gt + 1;
            while let Some(close_rel) = lower[j..].find("</") {
                let cs = j + close_rel;
                let Some(ce_rel) = lower[cs..].find('>') else { return None };
                let ce = cs + ce_rel;
                let cname = &block[cs + 2..ce];
                let cname_local = cname.split(':').next_back().unwrap_or(cname);
                if cname_local.eq_ignore_ascii_case(tag) {
                    let mut text = block[gt + 1..cs].to_string();
                    if text.contains("<![CDATA[") {
                        text = text.replace("<![CDATA[", "").replace("]]>", "");
                    }
                    return Some(text.trim().to_string());
                }
                j = ce + 1;
            }
            return None;
        }
        i = gt + 1;
    }
    None
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

// ---------------------------------------------------------------------------
// Mock 后端（内存实现；A3/A4/A11–A14 双机模拟回归用）
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    pub struct MockDav {
        pub files: BTreeMap<String, Vec<u8>>,
        pub ops: Vec<String>,
        fail_next: Option<AppError>,
        /// 模拟真实服务器：etag 返回自身值而非内容 hash（覆盖"附件增量"回归）
        pub opaque_etag: bool,
    }

    impl MockDav {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn fail_next(&mut self, err: AppError) {
            self.fail_next = Some(err);
        }
        fn check_fail(&mut self) -> Result<(), AppError> {
            match self.fail_next.take() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    impl StorageBackend for MockDav {
        fn test_connection(&mut self) -> Result<TestResult, AppError> {
            Ok(TestResult { ok: true, message: "mock".into() })
        }

        fn list_all(&mut self) -> Result<Vec<RemoteEntry>, AppError> {
            let mut out: Vec<RemoteEntry> = Vec::new();
            for (p, data) in &self.files {
                out.push(RemoteEntry {
                    path: p.clone(),
                    is_dir: false,
                    etag: if self.opaque_etag {
                        format!("W/\"{}\"", data.len()) // 服务器自身口径，与 blake3 恒不相等
                    } else {
                        crate::library::hash_hex(data)
                    },
                    size: data.len() as u64,
                });
                let mut parent = p.as_str();
                while let Some(i) = parent.rfind('/') {
                    parent = &parent[..i];
                    if !out.iter().any(|e| e.path == parent) {
                        out.push(RemoteEntry { path: parent.to_string(), is_dir: true, etag: String::new(), size: 0 });
                    }
                }
            }
            Ok(out)
        }

        fn get(&mut self, path: &str) -> Result<Vec<u8>, AppError> {
            self.check_fail()?;
            self.files.get(path).cloned().ok_or_else(|| AppError::new("not_found", format!("mock 缺少 {path}")))
        }

        fn put(&mut self, path: &str, data: &[u8]) -> Result<String, AppError> {
            self.check_fail()?;
            self.ops.push(format!("put {path}"));
            self.files.insert(path.to_string(), data.to_vec());
            Ok(if self.opaque_etag {
                format!("W/\"{}\"", data.len())
            } else {
                crate::library::hash_hex(data)
            })
        }

        fn mv(&mut self, from: &str, to: &str) -> Result<(), AppError> {
            self.check_fail()?;
            if !self.files.contains_key(from) {
                return Err(AppError::new("not_found", format!("mock 缺少 {from}")));
            }
            if self.files.contains_key(to) {
                return Err(AppError::new("net_conflict", "MOVE 目标已存在（Overwrite:F）"));
            }
            let data = self.files.remove(from).unwrap();
            self.ops.push(format!("mv {from} -> {to}"));
            self.files.insert(to.to_string(), data);
            Ok(())
        }

        fn delete(&mut self, path: &str) -> Result<(), AppError> {
            self.check_fail()?;
            self.ops.push(format!("delete {path}"));
            self.files.remove(path);
            Ok(())
        }

        fn ensure_dir(&mut self, _path: &str) -> Result<(), AppError> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propfind_parses_namespaced_multistatus() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
<d:response><d:href>/dav/writer/docs/%E5%B0%8F%E8%AF%B4A/10-%E7%AC%AC%E4%B8%80%E7%AB%A0.md</d:href>
<d:propstat><d:prop><d:resourcetype/><d:getetag>&quot;abc123&quot;</d:getetag></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/dav/writer/docs/%E5%B0%8F%E8%AF%B4A/</d:href>
<d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;
        let entries = parse_propfind(xml);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "10-第一章.md");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].etag, "abc123");
        assert!(entries[1].is_dir);
    }

    #[test]
    fn propfind_parses_no_prefix() {
        let xml = "<multistatus><response><href>/a/b.md</href><propstat><prop><getetag>xyz</getetag></prop></propstat></response></multistatus>";
        let entries = parse_propfind(xml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "b.md");
        assert_eq!(entries[0].etag, "xyz");
    }

    #[test]
    fn percent_roundtrip_cjk() {
        let enc = encode_path("docs/小说A/10-第一章.md");
        assert!(enc.starts_with("docs/"));
        assert!(!enc.contains('小'));
        assert_eq!(decode_path(&enc), "docs/小说A/10-第一章.md");
    }

    #[test]
    fn extract_tag_handles_cdata_and_entity() {
        assert_eq!(extract_tag(r#"<d:getetag>&quot;abc&quot;</d:getetag>"#, "getetag").as_deref(), Some("&quot;abc&quot;"));
        assert_eq!(extract_tag(r#"<x:getetag><![CDATA["z"]]></x:getetag>"#, "getetag").as_deref(), Some(r#""z""#));
        assert_eq!(extract_tag(r#"<nope>1</nope>"#, "getetag"), None);
    }
}
