/**
 * PDF 渲染舱（FR-EXP-05，文档 4.x「离屏渲染舱」）：
 * 常驻隐藏窗口，接收 Rust 投递的 {title, body} 载荷（分块拉取，NFR-17），
 * 以 Paged.js Previewer 分页，完成后发事件通知 Rust 侧执行 WebView2 PrintToPdf。
 * 手动重入：每任务 new Previewer()，不复用旧实例（预研报告 2026-10-20）。
 */
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { Previewer } from "pagedjs";

/** 排版模板：@page 体系（页码/页眉 string-set）+ 与编辑器一致的正文风格 */
const TEMPLATE_CSS = `
@page {
  size: A4;
  margin: 25mm 22mm;
  @bottom-center { content: counter(page); font-size: 9pt; color: #8a857a; }
  @top-center { content: string(doc-title); font-size: 9pt; color: #8a857a; }
}
html { font-family: "LXGW WenKai", "霞鹜文楷", "Source Han Sans SC", "思源黑体", "Microsoft YaHei", serif; }
body { margin: 0; font-size: 11pt; line-height: 1.9; color: #26241f; }
h1.doc-title { font-size: 18pt; text-align: center; margin: 0 0 1.2em; string-set: doc-title content(text); }
h1, h2, h3, h4, h5, h6 { break-after: avoid; margin: 1.3em 0 0.5em; }
h1 { font-size: 16pt; } h2 { font-size: 14pt; } h3 { font-size: 12.5pt; }
p { margin: 0.25em 0; text-align: justify; }
blockquote { margin: 0.7em 1em; padding-left: 0.8em; color: #6f6a5f; border-left: 2.5pt solid #b08552; }
blockquote p { margin: 0.15em 0; }
ul, ol { margin: 0.4em 0; padding-left: 1.8em; }
li { margin: 0.15em 0; }
ul.task { list-style: none; padding-left: 0.3em; }
ul.task li { display: flex; gap: 0.5em; align-items: baseline; }
ul.task li > span:last-child { flex: 1; }
.taskbox { font-family: "Segoe UI Symbol", sans-serif; }
li p { margin: 0.05em 0; }
code { font-family: Consolas, "Cascadia Code", monospace; font-size: 0.9em; }
pre { background: #f5f4f1; border-radius: 4pt; padding: 8pt 12pt; font-size: 9.5pt; line-height: 1.6; white-space: pre-wrap; break-inside: auto; }
pre code { display: block; }
table { border-collapse: collapse; width: 100%; margin: 0.8em 0; font-size: 10pt; }
th, td { border: 0.5pt solid #b5b0a4; padding: 4pt 8pt; vertical-align: top; }
/* text-align 不在此强制 left：单元格 style（列对齐/居中）与 exportHtml 内嵌样式生效（FR-UI-03 同源） */
th { background: #f0efec; font-weight: 600; }
th p, td p { margin: 0; }
img { max-width: 100%; }
hr { border: none; border-top: 0.5pt solid #b5b0a4; margin: 1.2em 0; }
a { color: #966c3b; text-decoration: none; }
@media print {
  body { margin: 0; }
  .pagedjs_page { break-after: page; }
  .pagedjs_page:last-child { break-after: auto; }
}
`;

interface ChamberPayload {
  title: string;
  body: string;
}

let busy = false;

/** 分块拉取载荷（NFR-17：大内容 IPC 不整包传输；块按 UTF-8 字符边界切，保证每块可独立拼接） */
async function pullAll(): Promise<ChamberPayload> {
  const parts: string[] = [];
  let cursor = 0;
  for (;;) {
    const r = await invoke<{ chunk: string; next: number; done: boolean }>(
      "pdf_chamber_pull",
      { cursor },
    );
    parts.push(r.chunk);
    cursor = r.next;
    if (r.done) return JSON.parse(parts.join("")) as ChamberPayload;
  }
}

async function render(): Promise<void> {
  if (busy) return;
  busy = true;
  const host = document.getElementById("render")!;
  try {
    const { title, body } = await pullAll();
    host.replaceChildren();
    const source = document.createElement("div");
    const h = document.createElement("h1");
    h.className = "doc-title";
    h.textContent = title;
    source.appendChild(h);
    const content = document.createElement("div");
    content.innerHTML = body;
    source.appendChild(content);
    const flow = await new Previewer().preview(
      source,
      // polisher.add 只认 {url: cssText} 对象；纯字符串会被当作 URL fetch（404 后 @page 静默丢失）
      [{ [window.location.href]: TEMPLATE_CSS }],
      host,
    );
    const pages = flow.total ?? flow.pages?.length ?? 0;
    // 诊断：第 2 页 margin box 的伪元素内容（页码/页眉是否生成）
    const p2 = document.querySelectorAll(".pagedjs_page")[1];
    const box = (sel: string) => {
      const el = p2?.querySelector(sel + " .pagedjs_margin-content");
      return el ? getComputedStyle(el, "::after").content : "(无)";
    };
    await emit("pdf-chamber://status", {
      ok: true,
      pages,
      diag: {
        pn: box(".pagedjs_margin-bottom-center"),
        head: box(".pagedjs_margin-top-center"),
      },
    });
  } catch (e) {
    await emit("pdf-chamber://status", { ok: false, error: String(e) });
  } finally {
    busy = false;
  }
}

declare global {
  interface Window {
    __writerPdfRender: () => void;
  }
}
window.__writerPdfRender = () => void render();
void emit("pdf-chamber://ready", {});
