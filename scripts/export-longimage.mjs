/**
 * 长图导出（V1.3）：HTML → 整页 PNG（puppeteer-core 驱动本机 Chrome，与稿纸回归同基建）。
 * 用法：node scripts/export-longimage.mjs <html文件> <输出png> [宽度px，默认760]
 */
import { pathToFileURL } from "node:url";
import puppeteer from "puppeteer-core";

const [, , htmlPath, outPng, widthArg] = process.argv;
if (!htmlPath || !outPng) {
  console.error("usage: export-longimage.mjs <html> <png> [width]");
  process.exit(2);
}
const width = Number(widthArg) || 760;

const CANDIDATES = [
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
  process.env.LOCALAPPDATA + "/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
];
import { existsSync } from "node:fs";
const exe = CANDIDATES.find((p) => p && existsSync(p));
if (!exe) {
  console.error("NO_BROWSER: 未找到 Chrome/Edge（长图导出依赖本机浏览器）");
  process.exit(3);
}

const browser = await puppeteer.launch({
  executablePath: exe,
  headless: "new",
  args: ["--hide-scrollbars", "--force-device-scale-factor=2", "--font-render-hinting=none"],
});
try {
  const page = await browser.newPage();
  await page.setViewport({ width, height: 800, deviceScaleFactor: 2 });
  await page.goto(pathToFileURL(htmlPath).href, { waitUntil: "networkidle0" });
  await page.screenshot({ path: outPng, fullPage: true });
  console.log("LONGIMAGE_OK");
} finally {
  await browser.close();
}
