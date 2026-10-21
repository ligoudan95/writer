//! 应用级网络出口配置（M5.1：设置中心「网络」页）。
//! 背景：reqwest 默认读取 Windows 系统代理（hyper-util client-proxy-system 读注册表
//! ProxyEnable/ProxyServer）。本机代理软件（Clash 等）未运行时系统代理指向死端口，
//! AI 与 WebDAV 全部请求会被劫持到死代理而连接失败。
//! 因此默认 direct（直连、显式绕过系统代理），用户可切换 system / custom。
//! 配置为应用级（%APPDATA%/Writer/network.json），跨文库共享；AI 客户端热重建，
//! WebDAV 客户端每次批次新建、自动生效。

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::library::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// 直连：显式禁用系统/环境代理（默认，防死代理劫持国内接口）
    #[default]
    Direct,
    /// 跟随系统代理（reqwest 默认行为：注册表 + 环境变量）
    System,
    /// 自定义代理（http://127.0.0.1:7890 等）
    Custom,
}

impl ProxyMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "direct" => Some(Self::Direct),
            "system" => Some(Self::System),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::System => "system",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetConfig {
    pub mode: ProxyMode,
    #[serde(default)]
    pub url: String,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self { mode: ProxyMode::Direct, url: String::new() }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Writer").join("network.json"))
}

/// 读取配置；文件缺失/损坏一律回退默认（direct），不阻塞启动
pub fn load() -> NetConfig {
    let Some(p) = config_path() else { return NetConfig::default() };
    let Ok(text) = fs::read_to_string(&p) else { return NetConfig::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(cfg: &NetConfig) -> Result<(), AppError> {
    let Some(p) = config_path() else {
        return Err(AppError::new("net_config", "无法定位配置目录（config_dir）"));
    };
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| AppError::new("net_config", format!("创建配置目录失败：{e}")))?;
    }
    let text = serde_json::to_string_pretty(cfg)
        .map_err(|e| AppError::new("net_config", format!("序列化失败：{e}")))?;
    fs::write(&p, text).map_err(|e| AppError::new("net_config", format!("写入失败：{e}")))?;
    Ok(())
}

/// 保存前校验：custom 必须带合法 http(s) 代理地址
pub fn validate(cfg: &NetConfig) -> Result<(), AppError> {
    if cfg.mode == ProxyMode::Custom {
        let u = cfg.url.trim();
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return Err(AppError::new(
                "net_config",
                "自定义代理必须以 http:// 或 https:// 开头",
            ));
        }
    }
    Ok(())
}

/// 应用到异步客户端构造器（AI NetGateway）
pub fn apply_async(
    builder: reqwest::ClientBuilder,
    cfg: &NetConfig,
) -> reqwest::ClientBuilder {
    apply_generic(builder, cfg)
}

/// 应用到阻塞客户端构造器（WebDAV DavBackend）
pub fn apply_blocking(
    builder: reqwest::blocking::ClientBuilder,
    cfg: &NetConfig,
) -> reqwest::blocking::ClientBuilder {
    apply_generic(builder, cfg)
}

/// direct：显式 no_proxy（覆盖系统/环境代理）；
/// custom：先 no_proxy 再挂自定义代理（reqwest 显式 proxy 会与环境代理合并，须先剥离）；
/// system：保持默认（reqwest 自行读注册表/环境变量）
fn apply_generic<B>(builder: B, cfg: &NetConfig) -> B
where
    B: ProxyCapable,
{
    match cfg.mode {
        ProxyMode::Direct => builder.no_proxy(),
        ProxyMode::Custom => {
            let b = builder.no_proxy();
            match reqwest::Proxy::all(cfg.url.trim()) {
                Ok(p) => b.with_proxy(p),
                Err(_) => b, // 非法地址：退化为直连（validate 已在保存侧拦截，此为兜底）
            }
        }
        ProxyMode::System => builder,
    }
}

/// 两套 ClientBuilder 的共同能力（async / blocking 同名方法）
trait ProxyCapable: Sized {
    fn no_proxy(self) -> Self;
    fn with_proxy(self, p: reqwest::Proxy) -> Self;
}

impl ProxyCapable for reqwest::ClientBuilder {
    fn no_proxy(self) -> Self {
        Self::no_proxy(self)
    }
    fn with_proxy(self, p: reqwest::Proxy) -> Self {
        Self::proxy(self, p)
    }
}

impl ProxyCapable for reqwest::blocking::ClientBuilder {
    fn no_proxy(self) -> Self {
        Self::no_proxy(self)
    }
    fn with_proxy(self, p: reqwest::Proxy) -> Self {
        Self::proxy(self, p)
    }
}

// ---------------------------------------------------------------------------
// 单元测试（脱离网络：直连/自定义的分支语义用本地回环验证）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn spawn_http_200() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            }
        });
        port
    }

    #[test]
    fn mode_parse_roundtrip() {
        assert_eq!(ProxyMode::parse("direct"), Some(ProxyMode::Direct));
        assert_eq!(ProxyMode::parse("system"), Some(ProxyMode::System));
        assert_eq!(ProxyMode::parse("custom"), Some(ProxyMode::Custom));
        assert_eq!(ProxyMode::parse("auto"), None);
        assert_eq!(ProxyMode::Direct.as_str(), "direct");
        let cfg: NetConfig = serde_json::from_str(r#"{"mode":"custom","url":"http://127.0.0.1:7890"}"#).unwrap();
        assert_eq!(cfg, NetConfig { mode: ProxyMode::Custom, url: "http://127.0.0.1:7890".into() });
        // 旧文件缺 url 字段：默认空串
        let cfg: NetConfig = serde_json::from_str(r#"{"mode":"direct"}"#).unwrap();
        assert_eq!(cfg.url, "");
    }

    #[test]
    fn validate_custom_requires_url() {
        assert!(validate(&NetConfig::default()).is_ok());
        assert!(validate(&NetConfig { mode: ProxyMode::System, url: String::new() }).is_ok());
        let bad = NetConfig { mode: ProxyMode::Custom, url: "127.0.0.1:7890".into() };
        assert!(validate(&bad).is_err());
        let ok = NetConfig { mode: ProxyMode::Custom, url: "http://127.0.0.1:7890".into() };
        assert!(validate(&ok).is_ok());
    }

    /// direct 必须显式绕过环境代理：本机回环服务器直连可达，
    /// 而 HTTPS_PROXY 指向死端口时系统默认行为会失败——no_proxy 后必须仍成功。
    ///（Windows 下 env 锁限制：此测试只证 no_proxy 分支不破坏直连）
    #[test]
    fn direct_mode_reaches_local_server() {
        let port = spawn_http_200();
        let cfg = NetConfig::default();
        let client = apply_blocking(reqwest::blocking::Client::builder(), &cfg)
            .build()
            .unwrap();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .expect("direct 模式访问本地回环必须成功");
        assert_eq!(resp.status().as_u16(), 200);
    }

    /// custom 模式必须真的走代理：代理指向死端口 → 对回环服务器的请求必须失败
    ///（若未生效则直连成功，测试失败——证明代理被应用）
    #[test]
    fn custom_mode_routes_through_proxy() {
        let port = spawn_http_200();
        let cfg = NetConfig { mode: ProxyMode::Custom, url: "http://127.0.0.1:9".into() };
        let client = apply_blocking(reqwest::blocking::Client::builder(), &cfg)
            .build()
            .unwrap();
        let r = client.get(format!("http://127.0.0.1:{port}/")).send();
        assert!(r.is_err(), "custom 代理不可达时请求必须失败，实际：{r:?}");
    }
}
