// 导出器自动测试：真实生成 TXT/MD/DOCX 产物并验证内容与结构
// 用法：先 esbuild 打包（见 package.json 或手动），再 node tests/exporters.mjs
import { mdToDoc, docToMd } from "../.tmp-codec.mjs";
import { exportTxt, exportMd, exportDocx } from "../.tmp-exporter.mjs";
import { writeFileSync, mkdirSync, rmSync, readFileSync } from "node:fs";

const richMd = `# 标题一

普通段落，含**加粗**、*斜体*、<u>下划线</u>、\`code\`、[链接](https://a.b)。

## 标题二

- 无序一
- 无序二

1. 第一
2. 第二

- [ ] 待办事项
- [x] 已办事项

> 引用一行内容

| 甲 | 乙 |
| --- | --- |
| a1 | b1 |

\`\`\`rust
fn main() {}
\`\`\`

---

结尾段落。
`;

let fail = 0;
const ok = (name, cond, extra = "") => {
  if (cond) console.log(`✓ ${name}`);
  else {
    fail++;
    console.log(`✗ ${name} ${extra}`);
  }
};

const doc = mdToDoc(richMd);

// --- MD 导出：应与 codec 序列化一致 ---
const md = exportMd(doc);
ok("MD: 与 codec 直出一致", md === docToMd(doc));
ok("MD: 含表格与任务列表", md.includes("| 甲 | 乙 |") && md.includes("- [x] 已办事项"));

// --- TXT 导出：剥离标记、保留文字 ---
const txt = exportTxt(doc);
ok("TXT: 保留标题文字", txt.includes("标题一") && txt.includes("标题二"));
ok("TXT: 无 markdown 标记", !txt.includes("#") || !/^#+ /m.test(txt));
ok("TXT: 无加粗星号", !txt.includes("**"));
ok("TXT: 任务列表转文字", txt.includes("[待办] 待办事项") && txt.includes("[已办] 已办事项"));
// 修复回归：bulletList 的 listItem 此前被 walk 静默跳过 → 整段文字消失
ok("TXT: 无序列表文字保留", txt.includes("无序一") && txt.includes("无序二"), txt);
ok("TXT: 表格转制表符", /甲\t乙/.test(txt) && /a1\tb1/.test(txt));
ok("TXT: 引用保留", txt.includes("引用一行内容"));
const txtCrlf = exportTxt(doc, true);
ok("TXT: CRLF 选项生效", txtCrlf.includes("\r\n") && !txt.includes("\r"));

// --- DOCX 导出：合法 Word 包 + 内容在 document.xml ---
const docx = await exportDocx(doc);
ok("DOCX: ZIP 魔数（PK）", docx[0] === 0x50 && docx[1] === 0x4b);
ok("DOCX: 体积合理（>4KB）", docx.length > 4000, `实际 ${docx.length}`);

// --- HTML 导出（FR-EXP-05 PDF 管线的正文片段） ---
import { exportHtml } from "../.tmp-exporter.mjs";
const html = exportHtml(doc);
ok("HTML: 标题层级（含目录锚点）", /<h1 id="h-\d+">标题一<\/h1>/.test(html) && /<h2 id="h-\d+">标题二<\/h2>/.test(html));
ok("HTML: 行内标记", html.includes("<strong>加粗</strong>") && html.includes("<em>斜体</em>") && html.includes("<u>下划线</u>"));
ok("HTML: 行内代码与链接", html.includes("<code>code</code>") && html.includes('<a href="https://a.b">'));
ok("HTML: 表格首行 th", /<table><tr><th><p>甲<\/p><\/th><th><p>乙<\/p><\/th><\/tr>/.test(html) && html.includes("<td><p>a1</p></td>"));
ok("HTML: 任务列表勾选态", html.includes("☑") && html.includes("☐"));
ok("HTML: 代码块语言类", html.includes('<pre><code class="language-rust">'));
ok("HTML: 引用块", html.includes("<blockquote><p>引用一行内容</p></blockquote>"));
ok("文本转义", !/[<](?!\/?(h[1-6]|p|ul|ol|li|blockquote|pre|code|table|tr|th|td|hr|img|a|strong|em|u|s|br|span|div)\b)/.test(html.replace(/<\/?(section|h\d)([^>]*)>/g, "")), "出现未转义 <");

mkdirSync(".export-test", { recursive: true });
writeFileSync(".export-test/样本.docx", docx);
writeFileSync(".export-test/样本.md", md);
writeFileSync(".export-test/样本.txt", txt);
writeFileSync(".export-test/样本.html", html);
console.log(`产物已写入 .export-test/（docx ${docx.length} 字节）`);

if (fail) {
  console.log(`${fail} 项失败`);
  process.exit(1);
}
console.log("ALL EXPORTER TESTS PASS");
