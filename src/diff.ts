/**
 * 选区 diff 分块（FR-AI-02 / R9 预研结论落地，见 docs/AI-Diff预研报告.md）。
 * 文本层 diff：token 化（CJK 逐字、西文/数字逐词、空白段独立）→ LCS → 修改块。
 * 纯函数、无编辑器依赖；应用侧由 store 以单事务替换选区（一次 Ctrl+Z 整体还原，AC A5）。
 */

export type DiffOpType = "eq" | "del" | "ins";

export interface DiffOp {
  type: DiffOpType;
  text: string;
}

/** 修改块：before/after 至少一侧非空；ctx* 为人工比对用的前后上下文 */
export interface DiffBlock {
  before: string;
  after: string;
  ctxBefore: string;
  ctxAfter: string;
}

// CJK：兼容汉字 + 假名；西文/数字连串；空白段。其余单字符各自成 token。
const TOKEN_RE = /[\u3400-\u4DBF\u4E00-\u9FFF\uF900-\uFAFF\u3040-\u30FF]|[A-Za-z0-9]+|\s+|[^\s]/y;

export function tokenize(s: string): string[] {
  const out: string[] = [];
  TOKEN_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = TOKEN_RE.exec(s)) !== null) {
    out.push(m[0]);
  }
  return out;
}

/** LCS 动态规划：长度表（Uint32）+ 方向表（0=↖ 相等 1=↑ 删 2=← 插） */
function lcsOps(a: string[], b: string[]): DiffOp[] {
  const n = a.length;
  const mm = b.length;
  const w = mm + 1;
  const len = new Uint32Array((n + 1) * w);
  const dir = new Uint8Array((n + 1) * w);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = mm - 1; j >= 0; j--) {
      const idx = i * w + j;
      if (a[i] === b[j]) {
        dir[idx] = 0;
        len[idx] = len[(i + 1) * w + (j + 1)] + 1;
      } else {
        const up = len[(i + 1) * w + j];
        const left = len[i * w + (j + 1)];
        if (up >= left) {
          dir[idx] = 1;
          len[idx] = up;
        } else {
          dir[idx] = 2;
          len[idx] = left;
        }
      }
    }
  }
  const ops: DiffOp[] = [];
  const push = (type: DiffOpType, text: string) => {
    const last = ops[ops.length - 1];
    if (last && last.type === type) last.text += text;
    else ops.push({ type, text });
  };
  let i = 0;
  let j = 0;
  while (i < n && j < mm) {
    const d = dir[i * w + j];
    if (d === 0) {
      push("eq", a[i]);
      i++;
      j++;
    } else if (d === 1) {
      push("del", a[i]);
      i++;
    } else {
      push("ins", b[j]);
      j++;
    }
  }
  while (i < n) push("del", a[i++]);
  while (j < mm) push("ins", b[j++]);
  return ops;
}

/** 超过此 token 积（防大选段 O(n·m) 失控）走整段替换回退 */
const MAX_CELLS = 4_000_000;

/** 全量 diff：返回 op 序列；超限时回退为单一替换 op（UI 呈现为 1 个块） */
export function diffOps(a: string, b: string): DiffOp[] {
  if (a === b) return [{ type: "eq", text: a }];
  const ta = tokenize(a);
  const tb = tokenize(b);
  if (ta.length * tb.length > MAX_CELLS) {
    return [...(a ? [{ type: "del" as DiffOpType, text: a }] : []), ...(b ? [{ type: "ins" as DiffOpType, text: b }] : [])];
  }
  return lcsOps(ta, tb);
}

/** 相邻修改块之间等值段 ≤ 此长度时合并为一块（块体验优先于字符级最优） */
const MERGE_GAP = 3;

type Run = { type: "eq"; text: string } | { type: "chg"; before: string; after: string };

/** ops → 分组 run：连续变更聚块，被极短等值段隔开的相邻块合并 */
function toRuns(ops: DiffOp[]): Run[] {
  const runs: Run[] = [];
  for (const op of ops) {
    if (op.type === "eq") {
      const last = runs[runs.length - 1];
      if (last && last.type === "eq") last.text += op.text;
      else runs.push({ type: "eq", text: op.text });
    } else {
      const last = runs[runs.length - 1];
      if (last && last.type === "chg") {
        if (op.type === "del") last.before += op.text;
        else last.after += op.text;
      } else {
        runs.push({
          type: "chg",
          before: op.type === "del" ? op.text : "",
          after: op.type === "ins" ? op.text : "",
        });
      }
    }
  }
  // 合并：chg eq(≤gap) chg → 单块（等值段同时进入 before/after）
  let merged = true;
  while (merged) {
    merged = false;
    for (let i = 0; i + 2 < runs.length + 1; i++) {
      const a = runs[i];
      const e = runs[i + 1];
      const b = runs[i + 2];
      if (a && e && b && a.type === "chg" && e.type === "eq" && b.type === "chg" && e.text.length <= MERGE_GAP) {
        runs.splice(
          i,
          3,
          { type: "chg", before: a.before + e.text + b.before, after: a.after + e.text + b.after },
        );
        merged = true;
        break;
      }
    }
  }
  return runs;
}

/** op 序列 → 修改块（含前后上下文摘要，供人工比对） */
export function opsToBlocks(ops: DiffOp[], ctxLen = 12): DiffBlock[] {
  let beforeAll = "";
  for (const op of ops) {
    if (op.type === "eq" || op.type === "del") beforeAll += op.text;
  }
  const runs = toRuns(ops);
  const blocks: DiffBlock[] = [];
  let bPos = 0;
  for (const r of runs) {
    if (r.type === "eq") {
      bPos += r.text.length;
      continue;
    }
    const start = bPos;
    bPos += r.before.length;
    blocks.push({
      before: r.before,
      after: r.after,
      ctxBefore: beforeAll.slice(Math.max(0, start - ctxLen), start),
      ctxAfter: beforeAll.slice(bPos, bPos + ctxLen),
    });
  }
  return blocks;
}

/**
 * 按接受标记重建文本：accepted[i] 对应 opsToBlocks 输出的第 i 块。
 * 未接受块保留原文（before），接受块采用新文（after）；等价段原样保留。
 */
export function applyBlocks(ops: DiffOp[], accepted: boolean[]): string {
  let out = "";
  let blockIdx = 0;
  for (const r of toRuns(ops)) {
    if (r.type === "eq") {
      out += r.text;
    } else {
      const take = accepted[blockIdx++] ?? false;
      out += take ? r.after : r.before;
    }
  }
  return out;
}
