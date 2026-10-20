# Paged.js 分页预研报告（FR-EXP-05 / R5）

- 日期：2026-10-20（M2 预研项）
- 结论：**可行。P1 高级排版（页码、页眉脚、目录页码、封面页）全部落地，建议 FR-EXP-05 P1 范围维持不变。**
- 复现：`node tests/pdf-spike/spike.mjs`（`SECTIONS=300 MIN_PAGES=150` 跑 10 万字压测）
- 产物：`.export-test/spike.pdf`（A4，207 页，1.3MB）
- **应用内已落地（2026-10-20 同日）**：渲染舱 `src/pdfchamber.ts` + COM 打印 `src-tauri/src/pdf.rs`；端到端验收 `cargo run -- --pdf-selftest <out.pdf>`（页码/页眉/内容/A4 尺寸全过，见 tests 与 exporters.mjs）

## 验证矩阵（无头 Chromium 131，与 WebView2 同引擎）

| 能力 | 模板语法 | 验证方式 | 结果 |
|---|---|---|---|
| 分页 + CJK 断行 | `@page { size: A4; margin }` | DOM 页数 == PDF 页数（43 页与 207 页两档均恒等） | ✓ |
| 页码 | `@bottom-center { content: counter(page) }` | PDF 文本层第 N 页含数字 N（2–5 页序列核对） | ✓ |
| 页眉（running header） | `string-set: doc-title` + `@top-center { content: string(doc-title) }` | PDF 文本层含标题串 | ✓ |
| 封面页遮蔽 | `@page :first { @top-center/@bottom-center { content: none } }` | 首页 0 数字、无页眉（::after 计算值 none） | ✓ |
| 目录页码 | `a::after { content: '···' target-counter(attr(href), page) }` | PDF 文本层条目带真实页码 | ✓（注意见下） |
| 性能 | — | 43 页 842ms；207 页（约 10 万字）3.6s；页盒 1122.5px = A4@96dpi | ✓ |

## 已知限制与实现注意

1. **`leader()` 函数不可用**——pagedjs 未实现 CSS leader，目录点线用字符串（`' ··· '`）或 flex 点线样式替代。`target-counter` 本身可用。
2. **`Previewer.preview(styles)` 只认 `{url: cssText}` 对象**——传纯字符串会被当作 URL 去 fetch（失败后 @page 规则**静默丢失**，页码/页眉全部为空，分页退回 Letter 默认）。正确形态：`[{ [window.location.href]: CSS }]`（2026-10-20 应用内落地实测）。
3. **margin box 内容经伪元素渲染**：`.pagedjs_margin-content` 的 `textContent` 恒为空，自动化验证须读 `getComputedStyle(el, "::after")`（计数器显示为 `counter(page)` 计算形态）或直接验 PDF 文本层。产品测试采用后者（成品口径）。
4. **分页完成信号必须轮询等待**：大文档分页耗时远超 `load`/`networkidle`，用 `PagedConfig.after` 钩子置位标志位 + `waitForFunction` 等待；本预研 207 页档若不等待会打出半成品 PDF。
5. **`target-counter(attr(href))` 必须挂在含 `href` 的元素（`a`）上**，挂在 `li` 上整条 content 声明失效（静默 none）。
6. 分页在 DOM 主线程进行，10 万字约 3.6s——必须按文档既定方案在**离屏渲染舱** webview 中执行，不得占用编辑器 webview。
7. **pypdf 提取的中文可能是康熙部首码位**（⾃/⼀/⽑ 代替 自/一/毛），自动化验证须先 NFKC 归一。

## 应用内集成路径（关键结论）

- **wry 0.57 无 `print_to_pdf` API**（tauri-apps/wry#707 自 2022 年开放至今未实现，源码核实无此符号）。
- 可行路径（仅 Windows，与项目定位一致）：
  1. 前端：DocModel → 排版模板 HTML（复用本报告 `content.html` 的 @page 体系）；
  2. 离屏渲染舱：`WebviewWindowBuilder` 建不可见窗口（visible=false）加载模板，Paged.js 分页，`after` 钩子经 Tauri event 通知 Rust；
  3. Rust 侧：`webview.with_webview()` 取原生 `ICoreWebView2`，经 [`webview2` crate](https://lib.rs/crates/webview2) COM 绑定调 `PrintToPdf`（ICoreWebView2_16），落盘后回传路径。
- 备选（若 COM 绑定受阻）：渲染舱分页后用 `window.print()` 的静默变体不可行（WebView2 无静默 print DOM API），故 COM 路径为主案。

## 后续（M2 实现粒度）

- `exporter.ts` 增补 PDF 通道：模板生成 + 渲染舱调度 + 进度反馈（FR-EXP-05 AC：导出 MD/DOCX 同源转换）。
- 字体：渲染舱与编辑器同字体链（LXGW 文楷 → 思源黑体 → 雅黑），随包分发策略见开放问题清单。
