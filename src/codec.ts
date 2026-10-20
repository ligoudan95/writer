/**
 * DocumentCodec 初版（3.2 M11 / 6.4 规范）
 * 方言：CommonMark + GFM（表格/任务列表/删除线）+ 高亮 ==（未实现）+ 下划线 <u> 直写
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
          out.push(imageNode(first as Tokens.Image)); // 仅含一张图 → 块级图片
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
      case "html":
        break; // M1：跳过未知块级 HTML（降级规则见 6.4，未知块不丢弃正文——块级 HTML 无正文）
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

function imageNode(t: Tokens.Image): PMNode {
  return {
    type: "image",
    attrs: { src: t.href, alt: t.text ?? null, title: t.title ?? null },
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
      case "codespan":
        out.push(textNode((t as Tokens.Codespan).text ?? "", [{ type: "code" }]));
        break;
      case "link":
        out.push(
          ...inline((t as Tokens.Link).tokens ?? [], [
            ...cur(),
            { type: "link", attrs: { href: (t as Tokens.Link).href } },
          ]),
        );
        break;
      case "image":
        out.push({ ...imageNode(t as Tokens.Image), marks: cur().length ? cur() : undefined });
        break;
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
  const headerRow: PMNode = {
    type: "tableRow",
    content: t.header.map((c) => ({
      type: "tableHeader",
      content: [cellNode(c)],
    })),
  };
  const bodyRows: PMNode[] = t.rows.map((r) => ({
    type: "tableRow",
    content: r.map((c) => ({
      type: "tableCell",
      content: [cellNode(c)],
    })),
  }));
  return { type: "table", content: [headerRow, ...bodyRows] };
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
        const fence = code.includes("```") ? "~~~" : "```";
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
          s += "`" + raw.replace(/`/g, "'") + "`";
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
  return s;
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
      case "underline":
        out = `<u>${out}</u>`;
        break;
      case "link":
        out = `[${out}](${String(m.attrs?.href ?? "")})`;
        break;
    }
  }
  return out;
}

/** 保守转义：仅处理会破坏方言语法的字符（避免中文标点被过度转义） */
function escText(s: string): string {
  return s.replace(/([\\`*_\[\]~<>])/g, "\\$1");
}

function imgToMd(n: PMNode): string {
  const src = String(n.attrs?.src ?? "");
  const alt = String(n.attrs?.alt ?? "");
  const title = n.attrs?.title ? ` "${String(n.attrs.title)}"` : "";
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
          .map((l) => "  " + l)
          .join("\n"),
      );
    }
    if (ordered) idx++;
  }
  return lines.join("\n");
}

function tableToMd(t: PMNode): string {
  const rows = t.content ?? [];
  const lines: string[] = [];
  rows.forEach((row, ri) => {
    const cells = (row.content ?? []).map((cell) => {
      const paras = cell.content ?? [];
      const txt = paras
        .map((p) => (p.type === "paragraph" ? inlineToMd(p.content ?? []) : ""))
        .join("<br>");
      return txt.replace(/\|/g, "\\|").replace(/\n/g, "<br>");
    });
    lines.push("| " + cells.join(" | ") + " |");
    if (ri === 0) lines.push("|" + cells.map(() => " --- ").join("|") + "|");
  });
  return lines.join("\n");
}
