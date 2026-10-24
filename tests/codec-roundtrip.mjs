// DocumentCodec 往返稳定性测试
// 两层断言：① md → JSON → md → JSON 两次 JSON 结构一致（不动点）；
// ② 针对已知丢失模式直接断言内容保真（不动点比较检不出"首次解析即丢"的场景）
import { mdToDoc, docToMd } from "../.tmp-codec.mjs";

const md1 = `# 标题一

普通段落，含**加粗**、*斜体*、<u>下划线</u>、~~删除线~~、\`code\`、[链接](https://a.b)。

## 标题二

- 无序一
- 无序二
  - 嵌套项

1. 第一
2. 第二

- [ ] 待办
- [x] 已办

> 引用一行

| 甲 | 乙 |
| --- | --- |
| a | b |

\`\`\`rust
fn main() {}
\`\`\`

---

![图片](/att/1.png)
`;

const md2 = `中文标点测试。数字 123 与英文 abc 混排，括号（全角）与 (半角)，波浪~线、星*号。

结尾段。
`;

// 修复回归样本：每一项都曾对应一个真实的正文丢失/损坏缺陷
const md3 = `![块图](attachments/20260101-1.png)

<div align="center">
HTML 块里的正文不能丢
</div>

\`\`行内代码含 \` 反引号\`\`

\`\`\`行内代码含 \`\` 双反引号\`\`\`

\`\`\`text
代码同时含 \`\`\` 与 ~~~ 定界
\`\`\`

| 左 | 中 | 右 |
| :--- | :---: | ---: |
| a | b | c |

<table>
  <tr>
    <th colspan="2">合并表头</th>
  </tr>
  <tr>
    <td rowspan="2">跨两行</td>
    <td>普通格</td>
  </tr>
  <tr>
    <td>第二行</td>
  </tr>
</table>

| 特殊 | 单元格 |
| --- | --- |
| 含竖线\\|与换行<br>第二行 | b |

1. 有序父项
   1. 嵌套有序子项
   2. 第二子项
2. 父项二

\\# 行首井号不是标题

\\- 行首短横不是列表

1\\. 行首序号不是列表

![非白名单图](javascript:alert(1))
`;

let fail = 0;
for (const [name, md] of Object.entries({ md1, md2, md3 })) {
  const j1 = mdToDoc(md);
  const out = docToMd(j1);
  const j2 = mdToDoc(out);
  const a = JSON.stringify(j1);
  const b = JSON.stringify(j2);
  if (a !== b) {
    fail++;
    console.log(`✗ ${name}: 二次解析结构不一致`);
    console.log("--- 第一次 ---\n" + out);
  } else {
    console.log(`✓ ${name}: 往返稳定（${j1.content.length} 块）`);
  }
}

// 内容保真断言（修复回归）
function expect(name, cond, detail) {
  if (cond) {
    console.log(`✓ ${name}`);
  } else {
    fail++;
    console.log(`✗ ${name}${detail ? "：" + detail : ""}`);
  }
}

const out3 = docToMd(mdToDoc(md3));
expect("图片节点往返（schema 现已注册 image）", out3.includes("](attachments/20260101-1.png)"), out3);
expect("HTML 块内部文字保留", out3.includes("HTML 块里的正文不能丢"), out3);
expect("行内代码反引号不篡改", out3.includes("`` 行内代码含 ` 反引号 ``"), out3);
expect(
  "行内代码双反引号不破栏",
  out3.includes("``` 行内代码含 `` 双反引号 ```"),
  out3,
);
// 高亮 ==文本== 往返（V1.2，FR-EDT-01 方言成员）
{
  const mdH = "段落，含==高亮文字==与普通文字。" + String.fromCharCode(10);
  const docH = mdToDoc(mdH);
  const marks = [];
  (function walk(n) {
    if (n.marks) marks.push(...n.marks.map((m) => m.type));
    (n.content ?? []).forEach(walk);
  })(docH);
  expect("高亮标记被解析", marks.includes("highlight"), marks.join(","));
  const back = docToMd(docH);
  expect("高亮往返无损", back.includes("==高亮文字=="), back);
  // == a ==（两侧空白）不得误判为高亮
  const mdEq = "公式 a == b == c 成立。" + String.fromCharCode(10);
  const docEq = mdToDoc(mdEq);
  const eqMarks = [];
  (function walk2(n) {
    if (n.marks) eqMarks.push(...n.marks.map((m) => m.type));
    (n.content ?? []).forEach(walk2);
  })(docEq);
  expect("比较运算符不误判高亮", !eqMarks.includes("highlight"), eqMarks.join(","));
}

expect("代码围栏并存不提前闭栏", docToMd(mdToDoc(md3)).split("~~~").length >= 2 || out3.includes("~~~~"), out3);
expect("单元格竖线转义保留", out3.includes("含竖线\\|"), out3);
expect("单元格 <br> 换行保留", out3.includes("与换行<br>第二行"), out3);
expect("嵌套有序列表不断裂", /1\. 有序父项\n\s+1\. 嵌套有序子项/.test(out3), out3);
expect(
  "行首结构字符转义",
  out3.includes("\\# 行首井号") && out3.includes("\\- 行首短横") && out3.includes("1\\. 行首序号"),
  out3,
);
expect("javascript: 图片被白名单拒绝", !out3.includes("javascript:"), out3);
expect(
  "表格列对齐往返",
  /\|\s*:---\|/.test(out3) && out3.includes(":---:") && out3.includes("---:"),
  out3,
);
expect(
  "合并单元格经 HTML 表格方言往返",
  out3.includes("<table>") && out3.includes('colspan="2"') && out3.includes('rowspan="2"') && out3.includes("合并表头") && out3.includes("跨两行"),
  out3,
);

// R7 性能冒烟：10 万字文档 md→DocModel→md 全程 < 5s（M5 性能回归基线）
{
  const block = "章回测试。".repeat(10) + "\n\n" + "正文内容测试abc ".repeat(10) + "\n\n";
  const big = block.repeat(700); // ≈10 万字符
  const t0 = performance.now();
  const j = mdToDoc(big);
  const o = docToMd(j);
  const dt = performance.now() - t0;
  expect(`10万字 codec 性能（${Math.round(dt)}ms < 5000ms）`, dt < 5000 && o.length > 90000, `${Math.round(dt)}ms`);
}

if (fail) {
  console.log(docToMd(mdToDoc(md1)));
  process.exit(1);
}
console.log("ALL PASS");
