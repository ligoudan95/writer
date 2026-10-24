/**
 * 导出器（FR-EXP，M2）：DocModel → TXT / MD / DOCX / HTML(正文片段，PDF 渲染舱消费)
 * PDF 管线结论见 docs/PagedJS-预研报告.md。
 */
import { docToMd, type PMNode } from "./codec";
import {
  BorderStyle,
  Document,
  ExternalHyperlink,
  HeadingLevel,
  Packer,
  Paragraph,
  Table,
  TableCell,
  TableRow,
  TextRun,
} from "docx";

// ---------------------------------------------------------------------------
// 导出前处理：H1 去重（FR-DOC-05：正文首个 H1 与文档标题相同时不重复输出，各 Exporter 统一遵循）
// ---------------------------------------------------------------------------

export function dedupeTitleH1(doc: PMNode, title?: string): PMNode {
  const t = (title ?? "").trim();
  if (!t) return doc;
  // 规格语义是"正文首个 H1"（不一定是第一块：引言段在前也有效）
  const idx = (doc.content ?? []).findIndex(
    (n) => n.type === "heading" && Number(n.attrs?.level ?? 0) === 1,
  );
  if (idx < 0) return doc;
  const text = textOf(doc.content![idx]).trim();
  if (text !== t) return doc;
  return { ...doc, content: (doc.content ?? []).filter((_, i) => i !== idx) };
}

// ---------------------------------------------------------------------------
// TXT：剥离标记，保留标题文字（FR-EXP-02）
// ---------------------------------------------------------------------------

export function exportTxt(doc: PMNode, crlf = false): string {
  const lines: string[] = [];
  const walk = (nodes: PMNode[], prefix = "", quote = false) => {
    for (const n of nodes) {
      switch (n.type) {
        case "heading":
          lines.push(prefix + textOf(n));
          lines.push("");
          break;
        case "paragraph":
          lines.push(prefix + (quote ? "　" : "") + textOf(n));
          lines.push("");
          break;
        case "bulletList":
          walk(n.content ?? [], prefix + "· ");
          break;
        case "listItem":
          walk(n.content ?? [], prefix);
          break;
        case "orderedList":
          (n.content ?? []).forEach((item, i) => {
            const no = Number(n.attrs?.start ?? 1) + i; // 尊重 start（内容保真）
            lines.push(`${prefix}${no}. ${textOf(item)}`);
            lines.push("");
          });
          break;
        case "taskList":
          (n.content ?? []).forEach((item) => {
            const checked = item.attrs?.checked ? "[已办] " : "[待办] ";
            lines.push(prefix + checked + textOf(item));
            lines.push("");
          });
          break;
        case "blockquote":
          walk(n.content ?? [], prefix, true);
          break;
        case "codeBlock":
          lines.push((n.content ?? []).map((c) => c.text ?? "").join(""));
          lines.push("");
          break;
        case "table":
          for (const row of n.content ?? []) {
            const cells = (row.content ?? []).map((c) => textOf(c).replace(/\t/g, " "));
            lines.push(cells.join("\t"));
          }
          lines.push("");
          break;
        case "horizontalRule":
          lines.push("----------------------------------------");
          lines.push("");
          break;
        case "image":
          lines.push(`[图片：${String(n.attrs?.src ?? "")}]`);
          lines.push("");
          break;
      }
    }
  };
  walk(doc.content ?? []);
  let out = lines.join("\n").replace(/\n{3,}/g, "\n\n").trim() + "\n";
  if (crlf) out = out.replace(/\n/g, "\r\n");
  return out;
}

function textOf(node: PMNode): string {
  if (node.text) return node.text;
  return (node.content ?? []).map(textOf).join("");
}

// ---------------------------------------------------------------------------
// MD：方言直出（FR-EXP-03，M2 初版默认不含 frontmatter）
// ---------------------------------------------------------------------------

export function exportMd(doc: PMNode, frontmatter = ""): string {
  return frontmatter + docToMd(doc);
}

// ---------------------------------------------------------------------------
// HTML：正文片段（FR-EXP-05 PDF 管线；@page 模板在渲染舱 src/pdfchamber.ts）
// ---------------------------------------------------------------------------

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const escAttr = (s: string) => esc(s).replace(/"/g, "&quot;");

function inlineHtml(nodes: PMNode[]): string {
  let out = "";
  for (const n of nodes ?? []) {
    if (n.type === "text" && n.text) {
      let t = esc(n.text);
      const marks = n.marks ?? [];
      if (marks.some((m) => m.type === "code")) t = `<code>${t}</code>`;
      if (marks.some((m) => m.type === "bold")) t = `<strong>${t}</strong>`;
      if (marks.some((m) => m.type === "italic")) t = `<em>${t}</em>`;
      if (marks.some((m) => m.type === "underline")) t = `<u>${t}</u>`;
      if (marks.some((m) => m.type === "strike")) t = `<s>${t}</s>`;
      if (marks.some((m) => m.type === "highlight")) t = `<mark>${t}</mark>`;
      const link = marks.find((m) => m.type === "link");
      out += link ? `<a href="${escAttr(String(link.attrs?.href ?? ""))}">${t}</a>` : t;
    } else if (n.type === "hardBreak") {
      out += "<br>";
    } else if (n.type === "image") {
      out += `<img src="${escAttr(String(n.attrs?.src ?? ""))}" alt="">`;
    } else if (n.content) {
      out += inlineHtml(n.content);
    }
  }
  return out;
}

function listItemsHtml(items: PMNode[], kind: "ul" | "ol" | "task"): string {
  const li = (item: PMNode, i: number) => {
    const paras = (item.content ?? []).filter((c) => c.type === "paragraph");
    const nested = (item.content ?? []).filter((c) =>
      ["bulletList", "orderedList", "taskList"].includes(c.type),
    );
    const inner =
      paras.map((p) => `<p>${inlineHtml(p.content ?? [])}</p>`).join("") +
      nested.map(listHtml).join("");
    if (kind === "task") {
      const box = item.attrs?.checked ? "☑" : "☐";
      return `<li><span class="taskbox">${box}</span><span>${inner}</span></li>`;
    }
    return kind === "ol" ? `<li value="${i + 1}">${inner}</li>` : `<li>${inner}</li>`;
  };
  if (kind === "ul") return `<ul>${items.map((x) => li(x, 0)).join("")}</ul>`;
  if (kind === "ol") return `<ol>${items.map(li).join("")}</ol>`;
  return `<ul class="task">${items.map((x) => li(x, 0)).join("")}</ul>`;
}

function listHtml(n: PMNode): string {
  if (n.type === "orderedList") return listItemsHtml(n.content ?? [], "ol");
  if (n.type === "taskList") return listItemsHtml(n.content ?? [], "task");
  return listItemsHtml(n.content ?? [], "ul");
}

/** HTML 正文片段（PDF 渲染舱消费）。传 lt 时内嵌与屏幕同源的排版样式（FR-UI-03） */
function escapeHtml(t: string): string {
  return t.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

export interface PdfOptions {
  /** 封面页（书名居中大字 + 日期） */
  cover?: boolean;
  /** 目录页（H1/H2 列表 + target-counter 页码，Paged.js 渲染） */
  toc?: boolean;
  /** 文档标题（封面/目录用） */
  title?: string;
}

export function exportHtml(doc: PMNode, lt?: ExportLayout, pdf?: PdfOptions): string {
  const out: string[] = [];
  // 标题锚点（目录跳转用）：稳定序号
  const anchorOf = (i: number) => `h-${i}`;
  const tocItems: { lv: number; text: string; anchor: string }[] = [];
  if (lt) {
    out.push(
      `<style>.pm-body{font-family:${lt.fontFamily};font-size:${lt.fontSize}px;line-height:${lt.lineHeight};}` +
        `.pm-body p{margin:0 0 ${lt.paraGap}px;}${lt.indent ? ".pm-body p{text-indent:2em;}" : ""}</style>` +
        `<div class="pm-body">`,
    );
  }
  // 封面 + 目录（FR-EXP V1.2 高级排版；页码/页眉由渲染舱 @page 体系承担）
  if (pdf?.cover) {
    const d = new Date();
    out.push(
      `<section class="pdf-cover"><h1 class="cover-title">${escapeHtml(pdf.title ?? "无题")}</h1>` +
        `<p class="cover-date">${d.getFullYear()} 年 ${d.getMonth() + 1} 月</p></section>`,
    );
  }
  const headings: { lv: number; html: string }[] = [];
  for (const n of doc.content ?? []) {
    if (n.type === "heading") {
      const lv = Math.min(Math.max(Number(n.attrs?.level ?? 1), 1), 6);
      if (lv <= 2) tocItems.push({ lv, text: textOf(n), anchor: anchorOf(headings.length) });
      headings.push({ lv, html: inlineHtml(n.content ?? []) });
    } else {
      headings.push({ lv: 0, html: "" });
    }
  }
  if (pdf?.toc && tocItems.length) {
    out.push(`<section class="pdf-toc"><h1 class="toc-title">目录</h1><ul>`);
    for (const t of tocItems) {
      out.push(
        `<li class="lv${t.lv}"><a href="#${t.anchor}">${escapeHtml(t.text)}</a></li>`,
      );
    }
    out.push(`</ul></section>`);
  }
  let k = 0;
  for (const n of doc.content ?? []) {
    switch (n.type) {
      case "heading": {
        const lv = headings[k].lv;
        out.push(`<h${lv} id="${anchorOf(k)}">${headings[k].html}</h${lv}>`);
        k++;
        break;
      }
      case "paragraph":
        out.push(`<p>${inlineHtml(n.content ?? [])}</p>`);
        break;
      case "bulletList":
      case "orderedList":
      case "taskList":
        out.push(listHtml(n));
        break;
      case "blockquote":
        out.push(
          `<blockquote>${(n.content ?? []).map((p) => `<p>${inlineHtml(p.content ?? [])}</p>`).join("")}</blockquote>`,
        );
        break;
      case "codeBlock": {
        const code = esc((n.content ?? []).map((c) => c.text ?? "").join(""));
        const lang = String(n.attrs?.language ?? "").replace(/[^\w-]/g, "");
        out.push(`<pre><code${lang ? ` class="language-${lang}"` : ""}>${code}</code></pre>`);
        break;
      }
      case "table": {
        const cellAttrs = (cell: PMNode): string => {
          let s = "";
          if (cell.attrs?.colspan) s += ` colspan="${Number(cell.attrs.colspan)}"`;
          if (cell.attrs?.rowspan) s += ` rowspan="${Number(cell.attrs.rowspan)}"`;
          const style = String(cell.attrs?.style ?? "");
          if (/^text-align:(left|center|right)$/i.test(style)) s += ` style="${style}"`;
          return s;
        };
        const rows = (n.content ?? [])
          .map(
            (row, ri) =>
              `<tr>${(row.content ?? [])
                .map((cell) => {
                  const tag = ri === 0 && cell.type === "tableHeader" ? "th" : "td";
                  return `<${tag}${cellAttrs(cell)}>${(cell.content ?? [])
                    .map((p) => `<p>${inlineHtml(p.content ?? [])}</p>`)
                    .join("")}</${tag}>`;
                })
                .join("")}</tr>`,
          )
          .join("");
        out.push(`<table>${rows}</table>`);
        break;
      }
      case "horizontalRule":
        out.push("<hr>");
        break;
      case "image":
        out.push(`<img src="${escAttr(String(n.attrs?.src ?? ""))}" alt="">`);
        break;
    }
  }
  const html = out.join("\n");
  return lt ? html + "</div>" : html;
}

// ---------------------------------------------------------------------------
// DOCX（FR-EXP-04）：内容保真级排版；排版参数与屏幕同源（FR-UI-03 单源消费）
// ---------------------------------------------------------------------------

/** 排版 token 的导出子集（来自 layout.ts exportTokens） */
export interface ExportLayout {
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
  paraGap: number;
  indent: boolean;
}

/** CSS 字体列表取首个族名（docx 需要单一字体名） */
export function primaryFont(css: string): string {
  const m = css.split(",")[0]?.trim().replace(/^["']|["']$/g, "");
  return m || "Microsoft YaHei";
}

export async function exportDocx(doc: PMNode, lt?: ExportLayout): Promise<Uint8Array> {
  const sizeHalfPt = Math.round((lt?.fontSize ?? 17) * 2 * 0.75); // px→pt（×0.75）→ docx 半点
  const lineTwips = Math.round((lt?.lineHeight ?? 1.9) * 240);
  const font = lt ? primaryFont(lt.fontFamily) : undefined;
  const d = new Document({
    styles: {
      default: {
        document: {
          run: { font, size: sizeHalfPt },
          paragraph: {
            spacing: { line: lineTwips, after: Math.round((lt?.paraGap ?? 10) * 15) },
            ...(lt?.indent
              ? { indent: { firstLine: Math.round((lt.fontSize ?? 17) * 2 * 15) } }
              : {}),
          },
        },
      },
    },
    sections: [{ children: buildDocx(doc.content ?? [], lt) }],
  });
  const blob = await Packer.toBlob(d);
  return new Uint8Array(await blob.arrayBuffer());
}

const HEADINGS = [
  HeadingLevel.HEADING_1,
  HeadingLevel.HEADING_2,
  HeadingLevel.HEADING_3,
  HeadingLevel.HEADING_4,
  HeadingLevel.HEADING_5,
  HeadingLevel.HEADING_6,
];

function runsOf(nodes: PMNode[]): (TextRun | ExternalHyperlink)[] {
  const out: (TextRun | ExternalHyperlink)[] = [];
  for (const n of nodes ?? []) {
    if (n.type === "text" && n.text) {
      const marks = n.marks ?? [];
      const has = (t: string) => marks.some((m) => m.type === t);
      const run = new TextRun({
        text: n.text,
        bold: has("bold"),
        italics: has("italic"),
        underline: has("underline") ? {} : undefined,
        strike: has("strike"),
        font: has("code") ? "Consolas" : undefined,
        highlight: has("highlight") ? "yellow" : undefined,
      });
      const link = marks.find((m) => m.type === "link");
      if (link) {
        out.push(
          new ExternalHyperlink({
            children: [new TextRun({ text: n.text, style: "Hyperlink" })],
            link: String(link.attrs?.href ?? ""),
          }),
        );
      } else {
        out.push(run);
      }
    } else if (n.type === "hardBreak") {
      out.push(new TextRun({ break: 1 }));
    } else if (n.type === "image") {
      out.push(new TextRun({ text: `[图片：${String(n.attrs?.src ?? "")}]` }));
    } else if (n.content) {
      out.push(...runsOf(n.content));
    }
  }
  return out;
}

function buildDocx(nodes: PMNode[], lt?: ExportLayout): (Paragraph | Table)[] {
  const out: (Paragraph | Table)[] = [];
  // 首行缩进（FR-UI-03 排版型属性：DOCX 生效；2 字符 ≈ 字号×2）
  const firstLine = lt?.indent ? { firstLine: Math.round((lt?.fontSize ?? 17) * 2 * 15) } : undefined;
  const pushListItems = (items: PMNode[], kind: "bullet" | "ordered" | "task", level: number, listStart = 1) => {
    items.forEach((item, i) => {
      const paras = (item.content ?? []).filter((c) => c.type === "paragraph");
      const nested = (item.content ?? []).filter((c) =>
        ["bulletList", "orderedList", "taskList"].includes(c.type),
      );
      // 有序列表尊重 start 属性（内容保真：5. 续前文 不得重排为 1.）
      let mark = kind === "bullet" ? "• " : kind === "ordered" ? `${listStart + i}. ` : "";
      if (kind === "task") mark = item.attrs?.checked ? "☑ " : "☐ ";
      const indent = { left: 360 * (level + 1) };
      if (paras.length === 0 && nested.length === 0) {
        out.push(new Paragraph({ indent, children: [new TextRun(mark)] }));
      }
      paras.forEach((p, pi) => {
        out.push(
          new Paragraph({
            indent,
            ...(pi === 0 ? {} : (firstLine ?? {})),
            ...(kind === "bullet" ? { bullet: { level } } : {}),
            children: pi === 0 ? [new TextRun(mark), ...runsOf(p.content ?? [])] : runsOf(p.content ?? []),
          }),
        );
      });
      for (const nl of nested) {
        pushListItems(
          nl.content ?? [],
          nl.type === "orderedList" ? "ordered" : nl.type === "taskList" ? "task" : "bullet",
          level + 1,
          nl.type === "orderedList" ? Number(nl.attrs?.start ?? 1) : 1,
        );
      }
    });
  };

  for (const n of nodes) {
    switch (n.type) {
      case "heading": {
        const level = Math.min(Math.max(Number(n.attrs?.level ?? 1), 1), 6);
        out.push(new Paragraph({ heading: HEADINGS[level - 1], children: runsOf(n.content ?? []) }));
        break;
      }
      case "paragraph":
        out.push(new Paragraph({ ...(firstLine ?? {}), children: runsOf(n.content ?? []) }));
        break;
      case "bulletList":
        pushListItems(n.content ?? [], "bullet", 0);
        break;
      case "orderedList":
        pushListItems(n.content ?? [], "ordered", 0, Number(n.attrs?.start ?? 1));
        break;
      case "taskList":
        pushListItems(n.content ?? [], "task", 0);
        break;
      case "blockquote":
        for (const p of n.content ?? []) {
          out.push(
            new Paragraph({
              indent: { left: 480 },
              border: { left: { style: BorderStyle.SINGLE, size: 12, color: "B08552", space: 8 } },
              children: runsOf(p.content ?? []),
            }),
          );
        }
        break;
      case "codeBlock": {
        const code = (n.content ?? []).map((c) => c.text ?? "").join("");
        for (const line of code.replace(/\n$/, "").split("\n")) {
          out.push(
            new Paragraph({
              shading: { fill: "F2F0EA" },
              children: [new TextRun({ text: line, font: "Consolas", size: 21 })],
            }),
          );
        }
        break;
      }
      case "table": {
        const rows = (n.content ?? []).map((row, ri) => {
          const cells = (row.content ?? []).map(
            (cell) =>
              new TableCell({
                // 合并单元格与对齐透传（FR-EDT-09 / FR-UI-03：导出所见即所得）
                ...(cell.attrs?.colspan ? { columnSpan: Number(cell.attrs.colspan) } : {}),
                ...(cell.attrs?.rowspan ? { rowSpan: Number(cell.attrs.rowspan) } : {}),
                children: (cell.content ?? []).map((p) => {
                  const style = String(cell.attrs?.style ?? "");
                  const alignment = /right/i.test(style)
                    ? ("right" as const)
                    : /center/i.test(style)
                      ? ("center" as const)
                      : undefined;
                  return new Paragraph({
                    ...(alignment ? { alignment } : {}),
                    children: runsOf(p.content ?? []),
                  });
                }),
              }),
          );
          return new TableRow({ children: cells, tableHeader: ri === 0 });
        });
        out.push(new Table({ rows, width: { size: 100, type: "pct" } }));
        out.push(new Paragraph({ text: "" }));
        break;
      }
      case "horizontalRule":
        out.push(
          new Paragraph({
            border: { bottom: { style: BorderStyle.SINGLE, size: 6, color: "AAAAAA", space: 1 } },
            children: [],
          }),
        );
        break;
      case "image":
        out.push(new Paragraph({ children: [new TextRun(`[图片：${String(n.attrs?.src ?? "")}]`)] }));
        break;
    }
  }
  return out;
}
