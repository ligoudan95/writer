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
          walk(n.content ?? [], prefix + "  ");
          break;
        case "orderedList":
          (n.content ?? []).forEach((item, i) => {
            lines.push(`${prefix}${i + 1}. ${textOf(item)}`);
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

export function exportMd(doc: PMNode): string {
  return docToMd(doc);
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

export function exportHtml(doc: PMNode): string {
  const out: string[] = [];
  for (const n of doc.content ?? []) {
    switch (n.type) {
      case "heading": {
        const lv = Math.min(Math.max(Number(n.attrs?.level ?? 1), 1), 6);
        out.push(`<h${lv}>${inlineHtml(n.content ?? [])}</h${lv}>`);
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
        const rows = (n.content ?? [])
          .map(
            (row, ri) =>
              `<tr>${(row.content ?? [])
                .map((cell) => {
                  const tag = ri === 0 ? "th" : "td";
                  return `<${tag}>${(cell.content ?? [])
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
  return out.join("\n");
}

// ---------------------------------------------------------------------------
// DOCX（FR-EXP-04）：内容保真级排版
// ---------------------------------------------------------------------------

export async function exportDocx(doc: PMNode): Promise<Uint8Array> {
  const d = new Document({
    sections: [{ children: buildDocx(doc.content ?? []) }],
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

function buildDocx(nodes: PMNode[]): (Paragraph | Table)[] {
  const out: (Paragraph | Table)[] = [];
  const pushListItems = (items: PMNode[], kind: "bullet" | "ordered" | "task", level: number) => {
    items.forEach((item, i) => {
      const paras = (item.content ?? []).filter((c) => c.type === "paragraph");
      const nested = (item.content ?? []).filter((c) =>
        ["bulletList", "orderedList", "taskList"].includes(c.type),
      );
      let mark = kind === "bullet" ? "• " : kind === "ordered" ? `${i + 1}. ` : "";
      if (kind === "task") mark = item.attrs?.checked ? "☑ " : "☐ ";
      const indent = { left: 360 * (level + 1) };
      if (paras.length === 0 && nested.length === 0) {
        out.push(new Paragraph({ indent, children: [new TextRun(mark)] }));
      }
      paras.forEach((p, pi) => {
        out.push(
          new Paragraph({
            indent,
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
        out.push(new Paragraph({ children: runsOf(n.content ?? []) }));
        break;
      case "bulletList":
        pushListItems(n.content ?? [], "bullet", 0);
        break;
      case "orderedList":
        pushListItems(n.content ?? [], "ordered", 0);
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
                children: (cell.content ?? []).map(
                  (p) => new Paragraph({ children: runsOf(p.content ?? []) }),
                ),
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
