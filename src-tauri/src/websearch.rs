//! 联网搜索（FR-AI-07，V1.2）：搜索 Provider（Tavily / 博查 / 自定义），
//! 结果卡片（标题/摘要/链接）由前端呈现；结果不自动进入正文。
//! 流量全部经 NetGateway（netcfg 代理出口），Key 经 DPAPI 存储。

use serde::{Deserialize, Serialize};

use crate::library::AppError;

#[derive(Deserialize, Clone, Debug, serde::Serialize)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(serde::Serialize, Clone, Default)]
pub struct WebSearchConfigView {
    /// "" = 未配置；tavily | bocha | custom
    pub provider: String,
    pub url: String,
    pub has_key: bool,
}

const CFG_KEY: &str = "web_search";
const DEFAULTS: &[(&str, &str)] = &[
    ("tavily", "https://api.tavily.com/search"),
    ("bocha", "https://api.bochaai.com/v1/web-search"),
];

fn load_cfg(root: &std::path::Path) -> (String, String) {
    crate::search::open(root)
        .ok()
        .and_then(|c| {
            c.query_row("SELECT v FROM kv WHERE k = ?1", [CFG_KEY], |r| r.get::<_, String>(0))
                .ok()
        })
        .and_then(|v| serde_json::from_str::<(String, String)>(&v).ok())
        .unwrap_or_default()
}

pub fn config_view(root: &std::path::Path) -> WebSearchConfigView {
    let (provider, url) = load_cfg(root);
    WebSearchConfigView {
        provider,
        url,
        has_key: crate::secrets::get("websearch:key").ok().flatten().is_some(),
    }
}

/// 保存配置；key 非空时入 DPAPI（留空保持已存）
pub fn set_config(root: &std::path::Path, provider: &str, url: &str, key: Option<&str>) -> Result<WebSearchConfigView, AppError> {
    let p = provider.trim().to_string();
    if !p.is_empty() && !["tavily", "bocha", "custom"].contains(&p.as_str()) {
        return Err(AppError::new("websearch", "Provider 须为 tavily / bocha / custom"));
    }
    let mut u = url.trim().to_string();
    if !p.is_empty() && u.is_empty() {
        u = DEFAULTS
            .iter()
            .find(|(k, _)| *k == p)
            .map(|(_, v)| v.to_string())
            .unwrap_or_default();
    }
    if p == "custom" && !u.starts_with("https://") && !u.starts_with("http://") {
        return Err(AppError::new("websearch", "自定义端点须以 http(s):// 开头"));
    }
    let conn = crate::search::open(root)?;
    conn.execute(
        "INSERT OR REPLACE INTO kv(k, v) VALUES (?1, ?2)",
        [CFG_KEY, &serde_json::to_string(&(p, u)).unwrap_or_default()],
    )
    .map_err(|e| AppError::new("websearch", e.to_string()))?;
    if let Some(k) = key {
        if !k.trim().is_empty() {
            crate::secrets::set("websearch:key", k.trim())?;
        }
    }
    Ok(config_view(root))
}

/// 解析 Tavily 响应：{"results":[{"title","url","content"}]}
fn parse_tavily(body: &str) -> Result<Vec<WebResult>, AppError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        results: Vec<serde_json::Value>,
    }
    let r: R = serde_json::from_str(body).map_err(|e| AppError::new("websearch", format!("响应解析失败：{e}")))?;
    Ok(r.results
        .into_iter()
        .filter_map(|v| {
            let title = v.get("title")?.as_str()?.to_string();
            let url = v.get("url")?.as_str()?.to_string();
            let snippet = v.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
            Some(WebResult { title, url, snippet })
        })
        .collect())
}

/// 解析博查响应：{"code":200,"data":{"webPages":{"value":[{"name","url","snippet"}]}}}
fn parse_bocha(body: &str) -> Result<Vec<WebResult>, AppError> {
    #[derive(Deserialize)]
    struct Page {
        #[serde(default)]
        value: Vec<serde_json::Value>,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(default, rename = "webPages")]
        web_pages: Option<Page>,
    }
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        data: Option<Data>,
    }
    let r: R = serde_json::from_str(body).map_err(|e| AppError::new("websearch", format!("响应解析失败：{e}")))?;
    let vals = r
        .data
        .and_then(|d| d.web_pages)
        .map(|p| p.value)
        .unwrap_or_default();
    Ok(vals
        .into_iter()
        .filter_map(|v| {
            let title = v.get("name")?.as_str()?.to_string();
            let url = v.get("url")?.as_str()?.to_string();
            let snippet = v.get("snippet").and_then(|c| c.as_str()).unwrap_or("").to_string();
            Some(WebResult { title, url, snippet })
        })
        .collect())
}

/// 阻塞包装（命令层 spawn_blocking 调用；内部起单线程 tokio 跑异步内核）
pub fn search_blocking(root: &std::path::Path, query: &str) -> Result<Vec<WebResult>, AppError> {
    tauri::async_runtime::block_on(search(root, query))
}

/// 执行搜索（NetGateway 出口；tavily/custom 走 tavily 协议，bocha 走博查协议）
pub async fn search(root: &std::path::Path, query: &str) -> Result<Vec<WebResult>, AppError> {
    let (provider, url) = load_cfg(root);
    if provider.is_empty() {
        return Err(AppError::new("websearch", "未配置联网搜索 Provider（设置 → AI 接口 → 联网搜索）"));
    }
    let key = crate::secrets::get("websearch:key")?.unwrap_or_default();
    if key.is_empty() {
        return Err(AppError::new("websearch", "未配置联网搜索 API Key"));
    }
    let endpoint = if url.is_empty() {
        DEFAULTS
            .iter()
            .find(|(k, _)| *k == provider)
            .map(|(_, v)| v.to_string())
            .unwrap_or_default()
    } else {
        url
    };
    let client = crate::netcfg::apply_async(
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10)),
        &crate::netcfg::load(),
    )
    .build()
    .unwrap_or_default();

    let (resp, is_bocha) = match provider.as_str() {
        "bocha" => {
            let r = client
                .post(&endpoint)
                .bearer_auth(&key)
                .json(&serde_json::json!({ "query": query, "summary": true, "count": 8 }))
                .send()
                .await
                .map_err(|e| AppError::new("net", format!("搜索请求失败：{e}")))?;
            (r, true)
        }
        _ => {
            // tavily / custom（tavily 协议）
            let r = client
                .post(&endpoint)
                .json(&serde_json::json!({ "api_key": key, "query": query, "max_results": 8 }))
                .send()
                .await
                .map_err(|e| AppError::new("net", format!("搜索请求失败：{e}")))?;
            (r, false)
        }
    };
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| AppError::new("net", e.to_string()))?;
    if !(200..=299).contains(&status) {
        return Err(AppError::new(
            "net",
            format!("搜索服务返回 {status}：{}", &body[..body.len().min(120)]),
        ));
    }
    if is_bocha {
        parse_bocha(&body)
    } else {
        parse_tavily(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tavily_and_bocha_shapes() {
        let t = parse_tavily(r#"{"results":[{"title":"甲","url":"https://a","content":"摘要一"},{"title":"乙","url":"https://b"}]}"#).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].snippet, ""); // 缺 content 容错
        let b = parse_bocha(r#"{"code":200,"data":{"webPages":{"value":[{"name":"丙","url":"https://c","snippet":"片段"}]}}}"#).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].title, "丙");
        // 博查空数据不崩
        assert!(parse_bocha(r#"{"code":200}"#).unwrap().is_empty());
    }
}
