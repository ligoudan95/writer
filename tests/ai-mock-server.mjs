/**
 * 本地 OpenAI 兼容模拟服务（M4 自主验收用，非产品代码）：
 *   GET  /v1/models          → 模型列表
 *   POST /v1/chat/completions → SSE 流式（按输入内容区分润色/续写/讨论）
 */
import http from "node:http";

const POLISH_MAP = [
  ["写作", "创作"],
  ["软件", "应用"],
  ["直接开始输入", "即刻落笔"],
];

function polish(text) {
  let out = text;
  for (const [a, b] of POLISH_MAP) out = out.split(a).join(b);
  return out;
}

const server = http.createServer((req, res) => {
  if (req.method === "GET" && req.url.includes("/v1/models")) {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ data: [{ id: "mock-writer-1" }] }));
    return;
  }
  if (req.method === "POST" && req.url.includes("/v1/chat/completions")) {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      let lastUser = "";
      try {
        const j = JSON.parse(body);
        lastUser = [...j.messages].reverse().find((m) => m.role === "user")?.content ?? "";
      } catch {}
      let reply;
      if (lastUser.includes("【光标前文】")) {
        reply = "夜色渐深，他合上笔记本，把今日的段落又读了一遍。\n\n第二段：写作原来是这样一件安静的事。";
      } else if (lastUser.startsWith("（讨论上下文") || lastUser.length < 80) {
        reply = "这是本地模拟服务的回复：结构上建议把转折提前，让悬念贯穿全章。";
      } else {
        // 用户消息 = 指令 + 空行 + 选区；仅润色选区部分
        const parts = lastUser.split("\n\n");
        reply = polish(parts[parts.length - 1]);
      }
      res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
      // 按 2 字一段流式输出，验证前端流式消费
      const chunks = [];
      for (let i = 0; i < reply.length; i += 2) chunks.push(reply.slice(i, i + 2));
      let idx = 0;
      const timer = setInterval(() => {
        if (idx < chunks.length) {
          res.write(`data: ${JSON.stringify({ choices: [{ delta: { content: chunks[idx++] } }] })}\n\n`);
        } else {
          clearInterval(timer);
          res.write("data: [DONE]\n\n");
          res.end();
        }
      }, 5);
    });
    return;
  }
  res.writeHead(404);
  res.end("nope");
});
server.listen(8787, "127.0.0.1", () => console.log("MOCK_AI_READY http://127.0.0.1:8787"));
