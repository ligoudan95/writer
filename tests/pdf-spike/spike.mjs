// Paged.js 分页预研驱动：分页 → DOM 断言 → 打印 PDF → 双向核对页数
import puppeteer from "puppeteer-core";
import { readFileSync, mkdirSync } from "node:fs";

const exe = "C:/Program Files/Google/Chrome/Application/chrome.exe";
const browser = await puppeteer.launch({ executablePath: exe, headless: "new" });
const page = await browser.newPage();
const S = process.env.SECTIONS;
await page.goto(new URL("./content.html" + (S ? "?s=" + S : ""), import.meta.url).href, { waitUntil: "networkidle0" });

await page.waitForFunction(() => window.__pagedDone, { timeout: 180000 }); // 大文档分页可长达数十秒
const pagedMs = await page.evaluate(() => window.__pagedMs);

const info = await page.evaluate(() => {
  // 页码/页眉经伪元素渲染，textContent 恒空；读 hasContent 类与 ::after 计算值
  const pages = [...document.querySelectorAll(".pagedjs_page")];
  const box = (i, sel) => {
    const el = pages[i]?.querySelector(sel);
    if (!el) return { cls: "(缺失)", after: "(缺失)" };
    const inner = el.querySelector(".pagedjs_margin-content") || el;
    return { cls: el.className.includes("hasContent") ? "hasContent" : "empty",
             after: getComputedStyle(inner, "::after").content };
  };
  return { pages: pages.length,
           pn5: box(4, ".pagedjs_margin-bottom-center"), header5: box(4, ".pagedjs_margin-top-center"),
           firstPn: box(0, ".pagedjs_margin-bottom-center"), firstHeader: box(0, ".pagedjs_margin-top-center"),
           bodyH: pages[0].getBoundingClientRect().height };
});

mkdirSync(".export-test", { recursive: true });
await page.pdf({ path: ".export-test/spike.pdf", format: "A4", printBackground: true, preferCSSPageSize: true, margin: { top: 0, bottom: 0, left: 0, right: 0 } });
await browser.close();

const pdf = readFileSync(".export-test/spike.pdf", "latin1");
const pdfPages = (pdf.match(/\/Type[\s]*\/Page[^s]/g) || []).length;
const pdfKB = Math.round(pdf.length / 1024);

// PDF 文本层验证：页码数字与页眉串必须真实落在成品里
const { extract } = await import("../lib/pdf-text.mjs");
const pdfTexts = await extract(".export-test/spike.pdf");

console.log(`分页耗时 ${pagedMs}ms | DOM 页数 ${info.pages} | PDF 页数 ${pdfPages} | ${pdfKB}KB | 页盒高 ${info.bodyH}px`);
console.log(`第5页页码盒=${info.pn5.cls}/${info.pn5.after} 页眉盒=${info.header5.cls}/${info.header5.after} | 首页页码=${info.firstPn.after} 页眉=${info.firstHeader.after}`);
console.log(`PDF 文本层: 第5页含页码"5"=${pdfTexts[4].includes("5")} 含页眉=${pdfTexts[4].includes("paged.js 分页预研样张")} | 首页页码=${(pdfTexts[0].match(/\d+/g) || []).length}个数字 | 第2页起页码序列=${[1, 2, 3, 4].every(i => pdfTexts[i].includes(String(i + 1)))}`);

const fails = [];
const MIN = Number(process.env.MIN_PAGES || 30);
if (info.pages < MIN) fails.push(`页数 ${info.pages} < ${MIN}`);
if (pdfPages !== info.pages) fails.push(`PDF 页数 ${pdfPages} ≠ DOM 页数 ${info.pages}`);
if (pdfPages !== pdfTexts.length) fails.push(`pypdf 提取页数 ${pdfTexts.length} ≠ ${pdfPages}`);
if (info.pn5.cls !== "hasContent") fails.push("第5页页码盒无内容");
if (info.header5.after !== '"paged.js 分页预研样张"') fails.push(`页眉 string-set 失效: ${info.header5.after}`);
if (info.firstPn.after !== "none" || info.firstHeader.after !== "none") fails.push("封面页 :first 未屏蔽页码/页眉");
if (!pdfTexts[4].includes("5")) fails.push("PDF 第5页文本层无页码 5");
if (!pdfTexts[4].includes("paged.js 分页预研样张")) fails.push("PDF 第5页文本层无页眉");
if (pdfTexts[0].includes("paged.js 分页预研样张") && pdfTexts[0].split("paged.js 分页预研样张").length > 2) fails.push("首页页眉未被遮蔽");
if (fails.length) { console.error("✗ " + fails.join("；")); process.exit(1); }
console.log("✓ Paged.js 预研全部断言通过（分页/页码/页眉/封面遮蔽/PDF 页数与文本层一致）");
