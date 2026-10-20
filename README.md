# Writer

本地优先的个人桌面写作软件。Windows 桌面端：Tauri 2（Rust 核心）+ Vue 3 + TypeScript + TipTap。

## 需求基线

全部需求、架构契约与验收标准见 [`docs/需求规格说明文档.md`](docs/需求规格说明文档.md)（v1.7.1 冻结版）。开发以该文档为唯一依据，正文已冻结，实现粒度事项见其附录 C。

## 开发

```bash
npm install        # 前端依赖
npm run tauri dev  # 开发模式（热重载）
npm run build      # 前端类型检查 + 构建
cargo check        # Rust 侧类型检查（在 src-tauri/ 下）
npm run tauri build # 打包安装器（NSIS）
```

## 测试

```bash
npm run test:grid        # 稿纸模式对齐回归（需本机 Chrome，经 puppeteer-core 驱动）
npm run build:tests      # 先生成 codec/exporter 测试打包件（.tmp-*.mjs，已 gitignore）
node tests/codec-roundtrip.mjs   # md ↔ DocModel 往返无损
node tests/exporters.mjs         # TXT/MD/DOCX/HTML 导出器（HTML 供 PDF 管线）
node tests/pdf-spike/spike.mjs   # Paged.js 分页预研（结论见 docs/PagedJS-预研报告.md）
cargo run -- --pdf-selftest out.pdf   # PDF 管线端到端自检（渲染舱分页 + WebView2 COM 打印）
cargo test                          # Rust 单测 + 同步双机模拟（A3/A4/A11–A15）+ AI 流式/取消/隐私
node tests/ai-diff.mjs              # 润色 diff 分块（需先 build:tests；M4/R9）
```

前置条件：Node 20+、Rust stable（MSVC）、Visual Studio C++ 工具集 + Windows 11 SDK、WebView2 运行时。

## 结构

```
docs/               需求规格说明文档（冻结基线）；PagedJS-预研报告
src/                前端（Vue 3）
  api.ts            类型化 command 门面（契约见文档 5.3）
  codec.ts          DocumentCodec：md ↔ TipTap JSON（文档 6.4）
  store.ts          应用状态 + 自动保存引擎 + 同步接线（事件/去抖/远端更新协议）
  exporter.ts       TXT/MD/DOCX 导出 + exportHtml（PDF 管线正文片段）
  pdfchamber.ts     PDF 渲染舱：离屏 webview 内 Paged.js 分页（FR-EXP-05）
  components/       文档树 / 编辑器 / 状态栏 / 导出面板 / 同步设置 / 冲突对比 / 首绑方向
src-tauri/          Rust 核心
  src/library.rs    LibraryService：单写者锁 + 原子写 + journal 崩溃恢复 + 回收站
  src/frontmatter.rs frontmatter 解析（保留未知键）
  src/pdf.rs        PDF 管线：渲染舱调度 + WebView2 PrintToPdf（COM 直调）
  src/dav.rs        WebDAV 存储后端（5.1 StorageBackend）+ Mock（双机模拟回归）
  src/secrets.rs    Secrets 服务：DPAPI 凭据加密（NFR-08）
  src/sync.rs       同步引擎：状态机批次 / 墓碑清单 / sync_state.db / 冲突三动作
  src/db.rs         meta.db：AI Provider / 对话 / 请求日志（仅本地）
  src/ai.rs         AI NetGateway：SSE 流式 / 真实取消 / 50ms 批量 ai:chunk / 错误归因
tests/              回归测试与预研驱动（稿纸 / codec / 导出器 / PDF spike / diff / AI 模拟服务）
```

## 核心原则（文档 3.3）

本地优先 · 数据可迁出（明文 md）· AI 可逆 · 心流优先（模态白名单制）· 库文件单写者 · 唯一网络出口。
