// DocumentCodec 往返稳定性测试：md → JSON → md → JSON，两次 JSON 必须结构一致
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

let fail = 0;
for (const [name, md] of Object.entries({ md1, md2 })) {
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
if (fail) {
  console.log(docToMd(mdToDoc(md1)));
  process.exit(1);
}
console.log("ALL PASS");
