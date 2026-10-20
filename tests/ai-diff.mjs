/**
 * 选区 diff 分块回归（FR-AI-02 / R9；AC A5 的逻辑基础）
 * 运行：npm run build:tests && node tests/ai-diff.mjs
 */
import { diffOps, opsToBlocks, applyBlocks, tokenize } from "../.tmp-diff.mjs";

let failed = 0;
function check(name, cond, detail = "") {
  if (cond) {
    console.log(`\x1b[32m✓\x1b[0m ${name}`);
  } else {
    failed++;
    console.error(`\x1b[31m✗ ${name} ${detail}\x1b[0m`);
  }
}

// 1. token 化口径
check("tokenize：CJK 逐字、西文逐词", JSON.stringify(tokenize("你好abc 12")) === JSON.stringify(["你", "好", "abc", " ", "12"]));

// 2. 无变化 → 单 eq、零块
{
  const ops = diffOps("完全相同的内容", "完全相同的内容");
  check("无变化零块", opsToBlocks(ops).length === 0);
}

// 3. 纯删除 / 纯插入
{
  const ops = diffOps("今天天气很好", "今天很好");
  const blocks = opsToBlocks(ops);
  check("纯删除出 1 块", blocks.length === 1 && blocks[0].before === "天气" && blocks[0].after === "", JSON.stringify(blocks));
}
{
  const ops = diffOps("开头和结尾", "开头和新的结尾");
  const blocks = opsToBlocks(ops);
  check("纯插入出 1 块", blocks.length === 1 && blocks[0].before === "" && blocks[0].after === "新的", JSON.stringify(blocks));
}

// 4. 替换（改）
{
  const ops = diffOps("他走到门口", "他跑到了门口");
  const blocks = opsToBlocks(ops);
  check("替换聚为 1 块", blocks.length === 1 && blocks[0].before === "走到" && blocks[0].after === "跑到了", JSON.stringify(blocks));
}

// 5. 多块 + 半份接受（AC A5 语义）
{
  const original = "第一段保持。甲句要改。中间也不变。乙句也要改。结尾原样。";
  const polished = "第一段保持。甲句已改！中间也不变。乙句已改！结尾原样。";
  const ops = diffOps(original, polished);
  const blocks = opsToBlocks(ops);
  check("两处修改出 2 块", blocks.length === 2, JSON.stringify(blocks));
  const half = applyBlocks(ops, [true, false]);
  check("半份接受：只应用第一块", half === "第一段保持。甲句已改！中间也不变。乙句也要改。结尾原样。", half);
  const all = applyBlocks(ops, [true, true]);
  check("全接受 = 润色后全文", all === polished, all);
  const none = applyBlocks(ops, [false, false]);
  check("全拒绝 = 原文", none === original);
}

// 6. CJK 与西文混排
{
  const ops = diffOps("使用 async 函数处理", "使用 await 函数处理");
  const blocks = opsToBlocks(ops);
  check("西文词级 diff", blocks.length === 1 && blocks[0].before === "async" && blocks[0].after === "await", JSON.stringify(blocks));
}

// 7. 性能冒烟：500 字选段（AC A5 规模）改一半
{
  const base = Array.from({ length: 500 }, (_, i) => "字" + (i % 10)).join("");
  const mod = base.split("");
  for (let i = 0; i < 250; i++) mod[i * 2] = "改";
  const t0 = performance.now();
  const ops = diffOps(base, mod.join(""));
  const blocks = opsToBlocks(ops);
  const applied = applyBlocks(ops, blocks.map(() => true));
  const ms = performance.now() - t0;
  check("500 字选段毫秒级", ms < 200, `${ms.toFixed(1)}ms`);
  check("全接受还原润色文", applied === mod.join(""));
}

if (failed) {
  console.error(`FAILED: ${failed}`);
  process.exit(1);
}
console.log("ALL DIFF TESTS PASS");
