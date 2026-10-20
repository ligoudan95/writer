// 稿纸模式对齐回归测试：渲染 tests/grid-harness.html，断言行网格不变量。
// 不变量（src/style.css 稿纸模式 v7，2026-10-20 定稿）：
//   1. 稿线光栅位于 y = 30 + 34k（padTop 34 与 background-position -4px 的相位差）；
//   2. 流式块（p/ul/blockquote/h1…）盒缘在 34 + 34k（块底与线下差 4px，文字坐线 gap 2px）；
//   3. 贴线块（pre/table，不透明底色）盒缘恰在光栅 30 + 34k 上，底距 +4px 恢复后续相位；
//   4. 一切块内文字片段底与下方最近稿线距离恒为 2px。
// 运行：node tests/grid-verify.mjs（需要本机 Chrome）
import puppeteer from "puppeteer-core";

const exe = "C:/Program Files/Google/Chrome/Application/chrome.exe";
const ROW = 34;
const RASTER = 30; // 光栅相位（30 + 34k 为稿线）

const browser = await puppeteer.launch({ executablePath: exe, headless: "new", args: ["--window-size=1296,1000", "--hide-scrollbars", "--font-render-hinting=none"] });
const page = await browser.newPage();
await page.setViewport({ width: 1296, height: 1000 });
await page.goto(new URL("./grid-harness.html", import.meta.url).href);

const data = await page.evaluate((ROW) => {
  const pm = document.querySelector(".ProseMirror");
  const blocks = [];
  for (const el of pm.children) {
    const r = el.getBoundingClientRect();
    const rec = { tag: el.tagName, top: r.top, bottom: r.bottom, h: r.height, texts: [] };
    for (const t of el.querySelectorAll("p, code")) {
      const rg = document.createRange();
      rg.selectNodeContents(t);
      for (const c of rg.getClientRects()) {
        if (c.height > 5 && c.width > 5) rec.texts.push(c.bottom);
      }
    }
    blocks.push(rec);
  }
  return blocks;
}, ROW);
await browser.close();

const failures = [];
const ok = (cond, msg) => { if (!cond) failures.push(msg); };
const nextLine = (y) => { const k = Math.ceil((y - RASTER + 0.01) / ROW); return RASTER + k * ROW; };
const mod = (a, m) => ((a % m) + m) % m;

for (const b of data) {
  const snapped = b.tag === "PRE" || b.tag === "TABLE";
  const phase = snapped ? RASTER : 34 % ROW; // 流式块相位 = 0（34+34k ≡ 0 mod 34）
  ok(Math.abs(mod(b.top - phase, ROW)) < 0.5 || Math.abs(mod(b.top - phase, ROW) - ROW) < 0.5,
     `${b.tag} top=${b.top.toFixed(1)} 相位不符（期望 ${snapped ? "30" : "0"} mod 34）`);
  ok(Math.abs(b.h / ROW - Math.round(b.h / ROW)) < 0.02, `${b.tag} 高 ${b.h.toFixed(1)} 非 34 整数倍`);
  for (const t of b.texts) {
    ok(Math.abs(nextLine(t) - t - 2) < 0.6, `${b.tag} 文字底 ${t.toFixed(1)} 坐线 gap=${(nextLine(t) - t).toFixed(1)}≠2`);
  }
}

// 相邻块衔接：贴线块自带上移 -4 顶距与 +4 底距，其余紧邻 0
for (let i = 0; i < data.length - 1; i++) {
  const a = data[i], b = data[i + 1];
  const exp = (a.tag === "PRE" || a.tag === "TABLE" ? 4 : 0) + (b.tag === "PRE" || b.tag === "TABLE" ? -4 : 0);
  ok(Math.abs(b.top - a.bottom - exp) < 0.5, `${a.tag}→${b.tag} 衔接 ${b.top - a.bottom}≠${exp}`);
}

if (failures.length) {
  console.error(`✗ 稿纸对齐回归失败（${failures.length} 项）:`);
  for (const f of failures) console.error("  - " + f);
  process.exit(1);
}
console.log(`✓ 稿纸对齐回归通过：${data.length} 块，盒缘相位 / 34px 整数倍 / 坐线 gap=2 / 衔接间距 全部符合`);
