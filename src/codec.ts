/**
 * DocumentCodec 初版（3.2 M11 / 6.4 规范）
 * 方言：CommonMark + GFM（表格/任务列表/删除线）+ 高亮 ==（V1.2）+ 下划线 <u> 直写
 * md ↔ ProseMirror(TipTap) JSON 双向转换，独立于编辑器实例（可无头使用）。
 */
import { marked, type Token, type Tokens } from "marked";

export interface PMMark {
  type: string;
  attrs?: Record<string, unknown>;
}
export interface PMNode {
  type: string;
  attrs?: Record<string, unknown>;
  content?: PMNode[];
  marks?: PMMark[];
  text?: string;
}

// ---------------------------------------------------------------------------
// md → ProseMirror JSON
// ---------------------------------------------------------------------------

// ==高亮== 内联扩展（方言成员）：marked 原生不支持，自定义 tokenizer；
// 起始定界符后必须紧跟非空白（与 ** 语义一致），防误吞 "a == b == c"
type HighlightToken = { type: "highlight"; raw: string; tokens: Token[] };
const highlightExt = {
  name: "highlight",
  level: "inline" as const,
  start(src: string) {
    return src.indexOf("==");
  },
  tokenizer(this: { lexer: { inlineTokens: (src: string, tokens: Token[]) => Token[] } }, src: string): HighlightToken | undefined {
    const m = /^==(?=[^\s=])([\s\S]*?\S)==/.exec(src);
    if (!m) return undefined;
    const token = {
      type: "highlight",
      raw: m[0],
      tokens: [] as Token[],
    } as HighlightToken;
    this.lexer.inlineTokens(m[1], token.tokens);
    return token;
  },
};
marked.use({ extensions: [highlightExt] });

export function mdToDoc(md: string): PMNode {
  const tokens = marked.lexer(md);
  return { type: "doc", content: blocks(tokens) };
}

function blocks(tokens: Token[]): PMNode[] {
  const out: PMNode[] = [];
  for (const t of tokens) {
    switch (t.type) {
      case "space":
        break;
      case "heading": {
        const h = t as Tokens.Heading;
        out.push({
          type: "heading",
          attrs: { level: Math.min(Math.max(h.depth, 1), 6) },
          content: inline(h.tokens ?? []),
        });
        break;
      }
      case "paragraph": {
        const p = t as Tokens.Paragraph;
        const first = p.tokens?.[0];
        if (p.tokens?.length === 1 && first && (first as Tokens.Image).type === "image") {
          // 块级图片以"仅含图片的段落"承载（image 为 inline 节点，schema 一致性）
          const img = imageNode(first as Tokens.Image);
          if (img) out.push({ type: "paragraph", content: [img] });
        } else {
          out.push({ type: "paragraph", content: inline(p.tokens ?? []) });
        }
        break;
      }
      case "blockquote":
        out.push({
          type: "blockquote",
          content: blocks((t as Tokens.Blockquote).tokens ?? []),
        });
        break;
      case "code": {
        const c = t as Tokens.Code;
        out.push({
          type: "codeBlock",
          attrs: { language: c.lang ?? null },
          content: [textNode(c.text ?? "")],
        });
        break;
      }
      case "hr":
        out.push({ type: "horizontalRule" });
        break;
      case "table":
        out.push(tableNode(t as Tokens.Table));
        break;
      case "list":
        out.push(listNode(t as Tokens.List));
        break;
      case "text": {
        // 列表项内的松散文本块（marked 以块级 text token 表示）
        const inner = (t as Tokens.Text).tokens ?? [t];
        out.push({ type: "paragraph", content: inline(inner) });
        break;
      }
      case "html": {
        // HTML 表格方言（单元格合并的序列化载体，FR-EDT-09）：
        // 合并单元格无法以 GFM 管道表格表达 → 整表降级为 <table> 直写（Typora 同款策略）
        const rawHtml = (t as Tokens.HTML).text ?? "";
        if (/^\s*<table/i.test(rawHtml)) {
          const table = parseHtmlTable(rawHtml);
          if (table) {
            // caption 保内容：转为一行段落（表格自身方言无 caption 槽位，不丢正文）
            const cap = /<caption[^>]*>([\s\S]*?)<\/caption>/i.exec(rawHtml);
            if (cap) {
              const capText = cap[1].replace(/<[^>]*>/g, "").trim();
              if (capText) out.push({ type: "paragraph", content: [textNode(capText)] });
            }
            out.push(table);
            break;
          }
        }
        // 未知块级 HTML：不丢正文——剥掉标签保留内部文字行（NFR-15：结构降级，内容保真）
        const text = rawHtml
          .replace(/<[^>]*>/g, "")
          .replace(/&lt;/g, "<")
          .replace(/&gt;/g, ">")
          .replace(/&quot;/g, '"')
          .replace(/&amp;/g, "&");
        const lines = text
          .split("\n")
          .map((l) => l.trim())
          .filter(Boolean);
        if (lines.length) out.push({ type: "paragraph", content: [textNode(lines.join(" "))] });
        break;
      }
      default:
        if ("tokens" in t && Array.isArray((t as unknown as { tokens: Token[] }).tokens)) {
          out.push(...blocks((t as unknown as { tokens: Token[] }).tokens));
        }
    }
  }
  return out;
}

function textNode(t: string, marks?: PMMark[]): PMNode {
  return marks?.length ? { type: "text", text: t, marks } : { type: "text", text: t };
}

/** URL 白名单（NFR-15 消毒）：仅放行 http(s)/mailto/data:image、Windows 盘符绝对路径
 * 与相对路径；javascript: 等一律拒绝（协议名内嵌 \t\r\n 会骗过正则，先剥离再判） */
function safeUrl(u: string | null | undefined): string {
  const s = String(u ?? "")
    .trim()
    .replace(/[\t\r\n]/g, "");
  if (!s) return "";
  if (/^[a-zA-Z]:[\\/]/.test(s)) return s; // D:\pics\x.png / D:/pics/x.png 本地绝对路径
  if (/^(https?:|mailto:)/i.test(s) || /^data:image\//i.test(s)) return s;
  if (/^[a-z][a-z0-9+.-]*:/i.test(s)) return ""; // 其它协议
  return s; // 相对路径 / 锚点
}

function imageNode(t: Tokens.Image): PMNode | null {
  const src = safeUrl(t.href);
  if (!src) return null; // 非法协议的图片整节点拒绝
  return {
    type: "image",
    attrs: { src, alt: t.text ?? null, title: t.title ?? null },
  };
}

/** 行内 token → 行内节点；<u>…</u> 以标记栈处理（方言：下划线直写 HTML） */
function inline(tokens: Token[], baseMarks: PMMark[] = []): PMNode[] {
  const out: PMNode[] = [];
  const htmlStack: PMMark[] = [];
  const cur = (): PMMark[] => [...baseMarks, ...htmlStack];
  for (const t of tokens) {
    switch (t.type) {
      case "text": {
        const raw = (t as Tokens.Text).text ?? "";
        if (raw) out.push(textNode(raw, cur()));
        break;
      }
      case "escape":
        out.push(textNode((t as unknown as { text: string }).text ?? "", cur()));
        break;
      case "strong":
        out.push(...inline((t as Tokens.Strong).tokens ?? [], [...cur(), { type: "bold" }]));
        break;
      case "em":
        out.push(...inline((t as Tokens.Em).tokens ?? [], [...cur(), { type: "italic" }]));
        break;
      case "del":
        out.push(...inline((t as Tokens.Del).tokens ?? [], [...cur(), { type: "strike" }]));
        break;
      case "highlight":
        out.push(...inline((t as unknown as HighlightToken).tokens ?? [], [...cur(), { type: "highlight" }]));
        break;
      case "codespan":
        out.push(textNode((t as Tokens.Codespan).text ?? "", [{ type: "code" }]));
        break;
      case "link": {
        const href = safeUrl((t as Tokens.Link).href);
        const inner = inline((t as Tokens.Link).tokens ?? [], href ? [...cur(), { type: "link", attrs: { href } }] : cur());
        out.push(...inner);
        break;
      }
      case "image": {
        const img = imageNode(t as Tokens.Image);
        if (img) out.push({ ...img, marks: cur().length ? cur() : undefined });
        break;
      }
      case "br":
        out.push({ type: "hardBreak" });
        break;
      case "html": {
        const raw = ((t as Tokens.HTML).raw ?? "").trim().toLowerCase();
        if (raw === "<u>") {
          htmlStack.push({ type: "underline" });
        } else if (raw === "</u>") {
          const i = htmlStack.map((m) => m.type).lastIndexOf("underline");
          if (i >= 0) htmlStack.splice(i, 1);
        } else if (raw === "<br>" || raw === "<br/>" || raw === "<br />") {
          out.push({ type: "hardBreak" }); // 表格单元格内的换行表示（往返依赖）
        } else if ("tokens" in t && Array.isArray((t as unknown as { tokens: Token[] }).tokens)) {
          out.push(...inline((t as unknown as { tokens: Token[] }).tokens, cur()));
        }
        break;
      }
      default:
        if ("tokens" in t && Array.isArray((t as unknown as { tokens: Token[] }).tokens)) {
          out.push(...inline((t as unknown as { tokens: Token[] }).tokens, cur()));
        }
    }
  }
  // 合并相邻同标记的 text 节点（escape token 会把 \~ \* 拆成独立节点，破坏往返一致性）
  const merged: PMNode[] = [];
  for (const n of out) {
    const last = merged[merged.length - 1];
    if (
      n.type === "text" &&
      last?.type === "text" &&
      JSON.stringify(last.marks ?? []) === JSON.stringify(n.marks ?? [])
    ) {
      last.text = (last.text ?? "") + (n.text ?? "");
    } else {
      merged.push(n);
    }
  }
  return merged;
}

function tableNode(t: Tokens.Table): PMNode {
  // 列对齐（GFM :---:）存入表级 attrs（FR-EDT-01 方言；序列化时还原分隔行）
  const align = (t.align ?? []).map((a) => a ?? null);
  const headerRow: PMNode = {
    type: "tableRow",
    content: t.header.map((c, i) => ({
      type: "tableHeader",
      attrs: align[i] ? { style: `text-align:${align[i]}` } : undefined,
      content: [cellNode(c)],
    })),
  };
  const bodyRows: PMNode[] = t.rows.map((r) => ({
    type: "tableRow",
    content: r.map((c, i) => ({
      type: "tableCell",
      attrs: align[i] ? { style: `text-align:${align[i]}` } : undefined,
      content: [cellNode(c)],
    })),
  }));
  return {
    type: "table",
    attrs: align.some((a) => a) ? { align } : undefined,
    content: [headerRow, ...bodyRows],
  };
}

/** 解析 <table> 直写（合并单元格方言）。结构可控（自产为主），正则解析足够 */
function parseHtmlTable(html: string): PMNode | null {
  const rows: PMNode[] = [];
  const trRe = /<tr[^>]*>([\s\S]*?)<\/tr>/gi;
  let tr: RegExpExecArray | null;
  while ((tr = trRe.exec(html))) {
    const cells: PMNode[] = [];
    const cellRe = /<(th|td)([^>]*)>([\s\S]*?)<\/\1>/gi;
    let cell: RegExpExecArray | null;
    while ((cell = cellRe.exec(tr[1]))) {
      const tag = cell[1].toLowerCase();
      const attrStr = cell[2];
      const inner = cell[3];
      const colspan = /colspan\s*=\s*"?(\d+)"?/i.exec(attrStr);
      const rowspan = /rowspan\s*=\s*"?(\d+)"?/i.exec(attrStr);
      const align = /text-align:\s*(center|right|left)/i.exec(attrStr);
      const attrs: Record<string, unknown> = {};
      if (colspan) attrs.colspan = Number(colspan[1]);
      if (rowspan) attrs.rowspan = Number(rowspan[1]);
      if (align) attrs.style = `text-align:${align[1]}`;
      const text = inner
        .replace(/<br\s*\/?>/gi, "\n")
        .replace(/<[^>]*>/g, "")
        .replace(/&lt;/g, "<")
        .replace(/&gt;/g, ">")
        .replace(/&amp;/g, "&");
      const paras = text
        .split("\n")
        .map((l) => l.trim())
        .filter(Boolean);
      cells.push({
        type: tag === "th" ? "tableHeader" : "tableCell",
        attrs: Object.keys(attrs).length ? attrs : undefined,
        content: paras.length
          ? paras.map((l) => ({ type: "paragraph", content: inline(inlineTokens(l)) }))
          : [{ type: "paragraph", content: [] }],
      });
    }
    if (cells.length) {
      rows.push({ type: "tableRow", content: cells });
    }
  }
  if (!rows.length) return null;
  return { type: "table", content: rows };
}

/** 单行文本 → 行内 token（HTML 表格单元格内容用） */
function inlineTokens(line: string): Token[] {
  const first = marked.lexer(line)[0] as Tokens.Paragraph | undefined;
  return first?.tokens ?? [{ type: "text", text: line } as unknown as Token];
}

function cellNode(c: Tokens.TableCell | string): PMNode {
  let toks: Token[];
  if (typeof c === "string") {
    const lexed = marked.lexer(c);
    const first = lexed[0] as Tokens.Paragraph | undefined;
    toks = first?.tokens ?? [];
  } else {
    toks = (c as unknown as { tokens?: Token[] }).tokens ?? [];
  }
  return { type: "paragraph", content: inline(toks) };
}

function listNode(t: Tokens.List): PMNode {
  const isTask = t.items.some((i) => i.task);
  const listType = t.ordered ? "orderedList" : isTask ? "taskList" : "bulletList";
  const attrs =
    t.ordered && typeof t.start === "number" && t.start !== 1 ? { start: t.start } : undefined;
  const items: PMNode[] = t.items.map((it) => {
    const content = blocks(it.tokens ?? []);
    if (listType === "taskList") {
      return { type: "taskItem", attrs: { checked: !!it.checked }, content };
    }
    return { type: "listItem", content };
  });
  return { type: listType, attrs, content: items };
}

// ---------------------------------------------------------------------------
// ProseMirror JSON → md
// ---------------------------------------------------------------------------

export function docToMd(doc: PMNode): string {
  const parts = blocksToMd(doc.content ?? []);
  if (!parts.length) return "";
  return parts.join("\n\n") + "\n";
}

function blocksToMd(nodes: PMNode[]): string[] {
  const out: string[] = [];
  for (const n of nodes) {
    switch (n.type) {
      case "heading": {
        const level = Math.min(Math.max(Number(n.attrs?.level ?? 1), 1), 6);
        out.push("#".repeat(level) + " " + inlineToMd(n.content ?? []));
        break;
      }
      case "paragraph": {
        const s = inlineToMd(n.content ?? []);
        if (s) out.push(s);
        break;
      }
      case "blockquote": {
        const inner = blocksToMd(n.content ?? []).join("\n\n");
        out.push(
          inner
            .split("\n")
            .map((l) => "> " + l)
            .join("\n"),
        );
        break;
      }
      case "codeBlock": {
        const code = (n.content ?? []).map((c) => c.text ?? "").join("");
        // 围栏选择：取正文中最长游程较短一侧的字符，长度 = 该游程 + 1（≥3），
        // 保证内容中 ``` 与 ~~~ 并存时也不提前闭栏
        const bt = longestRun(code, "`");
        const tl = longestRun(code, "~");
        const fence =
          bt >= tl ? "~".repeat(Math.max(3, tl + 1)) : "`".repeat(Math.max(3, bt + 1));
        const lang = String(n.attrs?.language ?? "") || "";
        out.push(`${fence}${lang}\n${code.replace(/\n$/, "")}\n${fence}`);
        break;
      }
      case "horizontalRule":
        out.push("---");
        break;
      case "bulletList":
        out.push(listToMd(n.content ?? [], false, 1, false));
        break;
      case "orderedList":
        out.push(listToMd(n.content ?? [], true, Number(n.attrs?.start ?? 1), false));
        break;
      case "taskList":
        out.push(listToMd(n.content ?? [], false, 1, true));
        break;
      case "table":
        out.push(tableToMd(n));
        break;
      case "image":
        out.push(imgToMd(n));
        break;
      default:
        if (n.content) out.push(...blocksToMd(n.content));
    }
  }
  return out;
}

function inlineToMd(nodes: PMNode[]): string {
  let s = "";
  for (const n of nodes) {
    switch (n.type) {
      case "text": {
        const raw = n.text ?? "";
        if (!raw) break;
        if (n.marks?.some((m) => m.type === "code")) {
          // 定界符长度 = 内容最长反引号游程 + 1：双反引号定界会被内容中的
          // 双反引号提前闭合（结构损坏），同围栏修复一个思路
          const run = longestRun(raw, "`");
          if (run > 0) {
            const fence = "`".repeat(run + 1);
            s += `${fence} ${raw} ${fence}`;
          } else {
            s += "`" + raw + "`";
          }
        } else {
          s += wrapByMarks(n.marks, escText(raw));
        }
        break;
      }
      case "hardBreak":
        s += "\\\n";
        break;
      case "image":
        s += imgToMd(n);
        break;
      default:
        if (n.content) s += wrapByMarks(n.marks, inlineToMd(n.content));
    }
  }
  return escapeLeading(s);
}

/** 行首结构字符转义：#-+ 与 数字./数字) 在行首会被重解析为标题/列表，需转义防结构漂移 */
function escapeLeading(s: string): string {
  return s
    .split("\n")
    .map((l) =>
      l.replace(/^([#+]|[-+]|\d+[.)])/, (tok) => {
        if (/^\d/.test(tok)) return tok.slice(0, -1) + "\\" + tok.slice(-1); // 1\. 2\)
        return "\\" + tok;
      }),
    )
    .join("\n");
}

function wrapByMarks(marks: PMMark[] | undefined, s: string): string {
  if (!marks?.length) return s;
  let out = s;
  for (const m of marks) {
    switch (m.type) {
      case "bold":
        out = `**${out}**`;
        break;
      case "italic":
        out = `*${out}*`;
        break;
      case "strike":
        out = `~~${out}~~`;
        break;
      case "highlight":
        out = `==${out}==`;
        break;
      case "underline":
        out = `<u>${out}</u>`;
        break;
      case "link":
        out = `[${out}](${mdUrl(String(m.attrs?.href ?? ""))})`;
        break;
    }
  }
  return out;
}

/** 保守转义：仅处理会破坏方言语法的字符（避免中文标点被过度转义） */
function escText(s: string): string {
  return s.replace(/([\\`*_\[\]~<>])/g, "\\$1");
}

function longestRun(s: string, ch: string): number {
  let max = 0;
  let cur = 0;
  for (const c of s) {
    if (c === ch) {
      cur++;
      if (cur > max) max = cur;
    } else {
      cur = 0;
    }
  }
  return max;
}

/** 链接/图片目标：含空格或括号时以 <…> 包裹，防语法断裂 */
function mdUrl(u: string): string {
  const s = String(u ?? "");
  if (/[()\s]/.test(s)) return `<${s.replace(/>/g, "\\>")}>`;
  return s;
}

function imgToMd(n: PMNode): string {
  const src = mdUrl(String(n.attrs?.src ?? ""));
  const alt = String(n.attrs?.alt ?? "").replace(/[[\]]/g, "\\$&");
  const title = n.attrs?.title ? ` "${String(n.attrs.title).replace(/"/g, '\\"')}"` : "";
  return `![${alt}](${src}${title})`;
}

function listToMd(items: PMNode[], ordered: boolean, start: number, task: boolean): string {
  const lines: string[] = [];
  let idx = start;
  for (const item of items) {
    let marker = ordered ? `${idx}. ` : "- ";
    if (task) marker = `- [${item.attrs?.checked ? "x" : " "}] `;
    const flat: string[] = [];
    const nested: string[] = [];
    for (const child of item.content ?? []) {
      if (
        child.type === "bulletList" ||
        child.type === "orderedList" ||
        child.type === "taskList"
      ) {
        nested.push(
          listToMd(
            child.content ?? [],
            child.type === "orderedList",
            Number(child.attrs?.start ?? 1),
            child.type === "taskList",
          ),
        );
      } else if (child.type === "paragraph") {
        flat.push(inlineToMd(child.content ?? []));
      } else {
        flat.push(...blocksToMd([child]));
      }
    }
    const first = flat.join("\n");
    lines.push(marker + first.replace(/\n/g, "\n" + " ".repeat(marker.length)));
    for (const nl of nested) {
      lines.push(
        nl
          .split("\n")
          // 嵌套缩进 = 父项 marker 宽（CommonMark 内容列），有序/任务列表项下 2 空格不够
          .map((l) => " ".repeat(marker.length) + l)
          .join("\n"),
      );
    }
    if (ordered) idx++;
  }
  return lines.join("\n");
}

function tableToMd(t: PMNode): string {
  const rows = t.content ?? [];
  // 合并单元格（colspan/rowspan）→ HTML 表格直写（GFM 管道无法表达，FR-EDT-09 方言）
  const hasMerge = rows.some((row) =>
    (row.content ?? []).some((c) => c.attrs?.colspan || c.attrs?.rowspan),
  );
  if (hasMerge) {
    return htmlTableToMd(rows);
  }
  const align =
    (t.attrs?.align as (string | null)[] | undefined) ?? deriveAlignFromCells(rows[0]);
  const lines: string[] = [];
  rows.forEach((row, ri) => {
    const cells = (row.content ?? []).map((cell) => {
      const paras = cell.content ?? [];
      const txt = paras
        .map((p) => (p.type === "paragraph" ? inlineToMd(p.content ?? []) : ""))
        .join("<br>")
        // hardBreak 输出 "\\\n"：先转 <br>（单元格换行的方言表示），再兜底其余裸换行
        .replace(/\\\n/g, "<br>");
      return txt.replace(/\|/g, "\\|").replace(/\n/g, "<br>");
    });
    lines.push("| " + cells.join(" | ") + " |");
    if (ri === 0) {
      const seps = cells.map((_, i) => {
        const a = align?.[i];
        if (a === "center") return ":---:";
        if (a === "right") return "---:";
        if (a === "left") return ":---";
        return " --- ";
      });
      lines.push("|" + seps.join("|") + "|");
    }
  });
  return lines.join("\n");
}

/** 无表级 align 时从首行单元格 style 推导列对齐（外来 md / 粘贴表 / 拆分场景，
 * 单元格 style 是唯一对齐来源——不推导会在 GFM 序列化时静默丢失"所见"） */
function deriveAlignFromCells(headerRow: PMNode | undefined): (string | null)[] | null {
  if (!headerRow) return null;
  const cols = (headerRow.content ?? []).map((cell) => {
    const m = /text-align:\s*(left|center|right)/i.exec(String(cell.attrs?.style ?? ""));
    return m ? m[1].toLowerCase() : null;
  });
  return cols.some((c) => c) ? cols : null;
}

/** 合并单元格表格 → <table> 直写（与 parseHtmlTable 成对） */
function htmlTableToMd(rows: PMNode[]): string {
  const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const lines: string[] = ["<table>"];
  rows.forEach((row, ri) => {
    lines.push("  <tr>");
    for (const cell of row.content ?? []) {
      const tag = ri === 0 && cell.type === "tableHeader" ? "th" : "td";
      const attrs: string[] = [];
      if (cell.attrs?.colspan) attrs.push(`colspan="${cell.attrs.colspan}"`);
      if (cell.attrs?.rowspan) attrs.push(`rowspan="${cell.attrs.rowspan}"`);
      // style 白名单：仅回写受支持的对齐值（任意 style 原样回写会被内嵌引号/尖括号破坏自产 HTML）
      const style = String(cell.attrs?.style ?? "");
      if (/^text-align:(left|center|right)$/i.test(style)) attrs.push(`style="${style}"`);
      const txt = (cell.content ?? [])
        .map((p) => (p.type === "paragraph" ? esc(inlineToMd(p.content ?? []).replace(/\\\n/g, "<br>")) : ""))
        .join("<br>");
      lines.push(`    <${tag}${attrs.length ? " " + attrs.join(" ") : ""}>${txt}</${tag}>`);
    }
    lines.push("  </tr>");
  });
  lines.push("</table>");
  return lines.join("\n");
}
