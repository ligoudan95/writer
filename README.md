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
npm run licenses   # 收集开源许可 → src-tauri/LICENSES/（构建时自动执行）
npm run tauri build # 打包 NSIS 安装器（per-user，含 LICENSES）
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
docs/               需求规格说明文档（冻结基线）；预研报告；各里程碑交付记录
src/                前端（Vue 3）
  api.ts            类型化 command 门面（契约见文档 5.3）
  codec.ts          DocumentCodec：md ↔ TipTap JSON（文档 6.4；表格对齐/合并单元格 HTML 方言）
  store.ts          应用状态 + 自动保存引擎 + 会话恢复 + 同步接线
  theme.ts/layout.ts 主题三档 / 排版 token 单源（编辑区与导出同源消费）
  notify.ts         通知中心 + 打扰治理（输入保护）
  exporter.ts       TXT/MD/DOCX 导出 + exportHtml（PDF 管线正文片段）
  pdfchamber.ts     PDF 渲染舱：离屏 webview 内 Paged.js 分页（FR-EXP-05）
  components/       文档树 / 编辑器 / 状态栏 / 设置中心 / 通知中心 / 回收站 / 帮助 / 同步 / 冲突 / AI
src-tauri/          Rust 核心
  src/library.rs    LibraryService：单写者锁（跨实例注册）+ 库锁 + 原子写 + journal + 回收站 + 快照 + 对账
  src/applog.rs     日志（7 天滚动，前后端同文件）
  src/backup.rs     每日滚动备份 + 诊断包（脱敏）
  src/docx_import.rs DOCX → Markdown 方言导入
  src/frontmatter.rs frontmatter 解析（保留未知键）
  src/pdf.rs        PDF 管线：渲染舱调度 + WebView2 PrintToPdf（COM 直调）
  src/dav.rs        WebDAV 存储后端（5.1）+ 安全闸 + Mock（双机模拟回归）
  src/secrets.rs    Secrets 服务：DPAPI 凭据加密（NFR-08）
  src/sync.rs       同步引擎：状态机批次 / 墓碑（restored_at 复活标记）/ 冲突三动作 / 附件增量
  src/db.rs         meta.db：AI Provider / 对话 / 请求日志（仅本地）
  src/ai.rs         AI NetGateway：SSE 字节缓冲流式 / 真实取消 / 50ms 批量 ai:chunk / 错误归因
scripts/            collect-licenses.mjs（FR-PKG-07 构建期许可收集）
tests/              回归测试与预研驱动
```

## 核心原则（文档 3.3）

本地优先 · 数据可迁出（明文 md）· AI 可逆 · 心流优先（模态白名单制）· 库文件单写者 · 唯一网络出口。
