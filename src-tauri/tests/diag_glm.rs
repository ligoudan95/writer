//! 实机诊断：复刻应用网络路径（reqwest rustls + no_proxy + connect_timeout + SSE 流）
//! 用法：WRITER_GLM_KEY=... cargo test diag_glm_live -- --ignored --nocapture

#[cfg(test)]
mod diag {
    use std::time::{Duration, Instant};

    #[tokio::test]
    #[ignore = "实机诊断"]
    async fn diag_glm_live() {
        let key = std::env::var("WRITER_GLM_KEY").unwrap_or_default();
        assert!(!key.is_empty(), "需要 WRITER_GLM_KEY");
        // 与 ai.rs build_client 完全一致
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        let body = serde_json::json!({
            "model": "glm-5.3",
            "messages": [
                {"role": "system", "content": "你是写作讨论助手，回答简洁、贴合创作语境。"},
                {"role": "user", "content": "查询李白的生平简介"}
            ],
            "stream": true
        });
        let t0 = Instant::now();
        let resp = client
            .post("https://open.bigmodel.cn/api/paas/v4/chat/completions")
            .bearer_auth(&key)
            .json(&body)
            .send()
            .await
            .expect("请求失败");
        println!("[{:>6.2}s] HTTP {} version={:?}", t0.elapsed().as_secs_f32(), resp.status(), resp.version());
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut n_reason = 0usize;
        let mut n_content = 0usize;
        let mut buf: Vec<u8> = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(30), stream.next()).await {
                Err(_) => {
                    println!("[{:>6.2}s] IDLE 30s —— 中止", t0.elapsed().as_secs_f32());
                    break;
                }
                Ok(None) => {
                    println!("[{:>6.2}s] 流结束", t0.elapsed().as_secs_f32());
                    break;
                }
                Ok(Some(Ok(chunk))) => {
                    let dt = t0.elapsed().as_secs_f32();
                    if n_reason == 0 && n_content == 0 {
                        // 打印首批原始字节（看协议与编码）
                        println!("[{:>6.2}s] 首块 {} 字节: {:?}", dt, chunk.len(), &chunk[..chunk.len().min(160)]);
                    }
                    buf.extend_from_slice(&chunk);
                    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=pos).collect();
                        let line = String::from_utf8_lossy(&line);
                        let line = line.trim();
                        if let Some(payload) = line.strip_prefix("data:") {
                            let payload = payload.trim();
                            if payload == "[DONE]" {
                                println!("[{:>6.2}s] [DONE]", dt);
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
                                let delta = &v["choices"][0]["delta"];
                                if delta.get("reasoning_content").is_some() {
                                    n_reason += 1;
                                }
                                if delta.get("content").is_some() {
                                    n_content += 1;
                                }
                            }
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    println!("[{:>6.2}s] 流错误: {e}", t0.elapsed().as_secs_f32());
                    break;
                }
            }
        }
        println!("合计：思考 chunk {n_reason} 个，正文 chunk {n_content} 个，总耗时 {:.2}s", t0.elapsed().as_secs_f32());
    }
}
