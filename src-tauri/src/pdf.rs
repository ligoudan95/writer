//! PDF 渲染舱（FR-EXP-05）：离屏 webview Paged.js 分页 + WebView2 PrintToPdf COM 直调。
//! 依据预研报告 docs/PagedJS-预研报告.md（2026-10-20）：wry 无 print_to_pdf，
//! 经 with_webview 取 ICoreWebView2，由 webview2-com 绑定调用
//! ICoreWebView2_16::PrintToPdf（PrintSettings 需 A4 + 零页边距，与 @page 分页盒对齐）。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Listener, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::library::{AppError, CmdResult};

/// A4 纸面（英寸，PrintToPdf 设置的计量单位）
const A4_W: f64 = 210.0 / 25.4;
const A4_H: f64 = 297.0 / 25.4;
/// 单任务锁 + 载荷仓 + 渲染舱就绪标记
pub struct PdfState {
    pub job_lock: Mutex<()>,
    pub payload: Mutex<Option<String>>,
    pub loaded: AtomicBool,
}

impl Default for PdfState {
    fn default() -> Self {
        Self {
            job_lock: Mutex::new(()),
            payload: Mutex::new(None),
            loaded: AtomicBool::new(false),
        }
    }
}

#[derive(serde::Serialize)]
pub struct PdfChunk {
    pub chunk: String,
    pub next: usize,
    pub done: bool,
}

/// 渲染舱载荷分块拉取（NFR-17：大内容 IPC 分块；按 UTF-8 字符边界切，块可无损拼接）
#[tauri::command]
pub fn pdf_chamber_pull(state: tauri::State<'_, PdfState>, cursor: usize) -> CmdResult<PdfChunk> {
    let guard = state.payload.lock().unwrap_or_else(|e| e.into_inner());
    let Some(text) = guard.as_deref() else {
        return Ok(PdfChunk { chunk: String::new(), next: cursor, done: true });
    };
    let bytes = text.as_bytes();
    if cursor >= bytes.len() {
        return Ok(PdfChunk { chunk: String::new(), next: cursor, done: true });
    }
    let mut end = (cursor + 64 * 1024).min(bytes.len());
    while end < bytes.len() && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end += 1;
    }
    Ok(PdfChunk {
        chunk: text.get(cursor..end).unwrap_or_default().to_string(),
        next: end,
        done: end >= bytes.len(),
    })
}

/// PDF 导出（FR-EXP-05）：body 为 exportHtml 产物，dest 为用户选定路径
#[tauri::command]
pub fn pdf_export(app: AppHandle, title: String, body: String, dest: String) -> CmdResult<String> {
    export_pdf(&app, &title, &body, &dest)?;
    Ok(dest)
}

pub fn export_pdf(app: &AppHandle, title: &str, body: &str, dest: &str) -> Result<(), AppError> {
    let state = app.state::<PdfState>();
    let _job = state.job_lock.lock().unwrap_or_else(|e| e.into_inner());

    let payload = serde_json::json!({ "title": title, "body": body }).to_string();
    *state.payload.lock().unwrap_or_else(|e| e.into_inner()) = Some(payload);

    // 先挂 ready 监听再建窗，避免页面加载完成事件先于监听注册而丢失
    if !state.loaded.load(Ordering::Acquire) {
        let ready = EventWait::new(app, "pdf-chamber://ready")?;
        ensure_chamber(app)?;
        ready.wait(Duration::from_secs(60))?;
        state.loaded.store(true, Ordering::Release);
    } else {
        // 已就绪，但窗口可能已被销毁（异常路径）：重建后必须重新等 ready，
        // 否则新窗未完成加载就 eval 渲染，状态事件永不到达 → 挂满 300s 超时
        let existed = app.get_webview_window("pdf-chamber").is_some();
        ensure_chamber(app)?;
        if !existed {
            let ready = EventWait::new(app, "pdf-chamber://ready")?;
            ready.wait(Duration::from_secs(60))?;
        }
    }

    let chamber = app
        .get_webview_window("pdf-chamber")
        .ok_or_else(|| AppError::new("pdf_chamber", "渲染舱不可用"))?;
    chamber
        .eval("window.__writerPdfRender && window.__writerPdfRender()")
        .map_err(|e| AppError::new("pdf_chamber", format!("触发渲染失败：{e}")))?;

    let status = EventWait::new(app, "pdf-chamber://status")?.wait(Duration::from_secs(300))?;
    if !status.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = status.get("error").and_then(|v| v.as_str()).unwrap_or("未知错误");
        return Err(AppError::new("pdf_render", format!("分页渲染失败：{err}")));
    }
    eprintln!("[pdf] 渲染舱状态: {status}");

    // 先落临时文件再改名，失败不留半截目标文件；任何失败路径都清理 .tmp 残留
    let tmp = format!("{dest}.tmp");
    let printed = print_to_pdf(app, Path::new(&tmp))
        .and_then(|_| std::fs::rename(&tmp, dest).map_err(|e| AppError::new("pdf_io", format!("写入目标失败：{e}"))));
    if printed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    printed?;
    Ok(())
}

/// 一次性事件等待：注册监听 → 收到首个事件（或超时），Drop 时自动注销
struct EventWait {
    app: AppHandle,
    id: tauri::EventId,
    rx: mpsc::Receiver<serde_json::Value>,
}

impl EventWait {
    fn new(app: &AppHandle, name: &str) -> Result<Self, AppError> {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(Some(tx));
        let id = app.listen(name, move |e| {
            let value = serde_json::from_str(e.payload()).unwrap_or(serde_json::Value::Null);
            if let Some(tx) = tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = tx.send(value);
            }
        });
        Ok(Self { app: app.clone(), id, rx })
    }

    fn wait(self, timeout: Duration) -> Result<serde_json::Value, AppError> {
        self.rx
            .recv_timeout(timeout)
            .map_err(|_| AppError::new("pdf_timeout", "渲染舱响应超时"))
    }
}

impl Drop for EventWait {
    fn drop(&mut self) {
        self.app.unlisten(self.id);
    }
}

/// 渲染舱窗口：离屏但 visible——完全隐藏的 WebView2 会被节流，影响分页进度
fn ensure_chamber(app: &AppHandle) -> Result<(), AppError> {
    if app.get_webview_window("pdf-chamber").is_some() {
        return Ok(());
    }
    WebviewWindowBuilder::new(app, "pdf-chamber", WebviewUrl::App("pdfchamber.html".into()))
        .title("PDF 渲染舱")
        .inner_size(794.0, 1123.0)
        .position(-20000.0, 0.0)
        .decorations(false)
        .resizable(false)
        .skip_taskbar(true)
        .visible(true)
        .build()
        .map_err(|e| AppError::new("pdf_chamber", format!("创建渲染舱失败：{e}")))?;
    Ok(())
}

fn print_to_pdf(app: &AppHandle, out: &Path) -> Result<(), AppError> {
    let chamber = app
        .get_webview_window("pdf-chamber")
        .ok_or_else(|| AppError::new("pdf_chamber", "渲染舱不可用"))?;
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let out = out.to_path_buf();
    chamber
        .with_webview(move |wv| {
            let _ = tx.send(print_via_com(&wv, &out));
        })
        .map_err(|e| AppError::new("pdf_print", format!("进入原生 Webview 失败：{e}")))?;
    match rx.recv_timeout(Duration::from_secs(180)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(AppError::new("pdf_print", format!("打印失败：{e}"))),
        Err(_) => Err(AppError::new("pdf_print", "打印超时（COM 回调未达）")),
    }
}

fn print_via_com(wv: &tauri::webview::PlatformWebview, out: &Path) -> Result<(), String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_16, ICoreWebView2Environment6, COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT,
    };
    use webview2_com::PrintToPdfCompletedHandler;
    use windows_core::{Interface, HSTRING};

    unsafe {
        let core = wv
            .controller()
            .CoreWebView2()
            .map_err(|e| format!("CoreWebView2: {e}"))?;
        let wv16: ICoreWebView2_16 = core.cast().map_err(|e| format!("cast WebView2_16: {e}"))?;
        let env6: ICoreWebView2Environment6 = wv
            .environment()
            .cast()
            .map_err(|e| format!("cast Environment6: {e}"))?;
        let settings = env6
            .CreatePrintSettings()
            .map_err(|e| format!("CreatePrintSettings: {e}"))?;
        let set = |r: windows_core::Result<()>, what: &str| r.map_err(|e| format!("{what}: {e}"));
        set(settings.SetOrientation(COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT), "SetOrientation")?;
        set(settings.SetScaleFactor(1.0), "SetScaleFactor")?;
        set(settings.SetShouldPrintBackgrounds(true), "SetShouldPrintBackgrounds")?;
        set(settings.SetShouldPrintHeaderAndFooter(false), "SetShouldPrintHeaderAndFooter")?;
        set(settings.SetPageWidth(A4_W), "SetPageWidth")?;
        set(settings.SetPageHeight(A4_H), "SetPageHeight")?;
        set(settings.SetMarginTop(0.0), "SetMarginTop")?;
        set(settings.SetMarginBottom(0.0), "SetMarginBottom")?;
        set(settings.SetMarginLeft(0.0), "SetMarginLeft")?;
        set(settings.SetMarginRight(0.0), "SetMarginRight")?;

        // 与 @page 分页盒对齐：纸面 A4、页边距全零，Paged.js 的页盒恰好铺满每张纸
        let path = HSTRING::from(out.as_os_str());
        let success = std::sync::Arc::new(AtomicBool::new(false));
        let flag = success.clone();
        PrintToPdfCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                wv16.PrintToPdf(&path, &settings, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |_hr: windows_core::Result<()>, ok: bool| {
                flag.store(ok, Ordering::Relaxed);
                Ok(())
            }),
        )
        .map_err(|e| format!("PrintToPdf: {e}"))?;
        if !success.load(Ordering::Relaxed) {
            return Err("PrintToPdf 返回 success=false".into());
        }
        Ok(())
    }
}

/// --pdf-selftest CLI 自检样张正文（覆盖全部块类型，≥2 页）
pub fn selftest_body() -> String {
    let paras: Vec<String> = (1..=12)
        .map(|i| format!("<p>第{i}段：渲染舱自检文本。稿纸模式的行网格与 PDF 分页同日完成，本段用于验证跨页断行、两端对齐与页眉页脚渲染是否一致。</p>"))
        .collect();
    format!(
        concat!(
            "<h2>第一章　管线自检</h2>",
            "{paras}",
            "<h2>第二章　表格与代码</h2>",
            "<table><tr><th>环节</th><th>实现</th></tr>",
            "<tr><td><p>分页</p></td><td><p>Paged.js Previewer（离屏舱）</p></td></tr>",
            "<tr><td><p>打印</p></td><td><p>ICoreWebView2_16::PrintToPdf</p></td></tr></table>",
            "<pre><code class=\"language-rust\">let flow = new Previewer().preview(src, [css], host);</code></pre>",
            "<blockquote><p>引用块：本地优先，数据可迁出。</p></blockquote>",
            "<ul><li><p>要点一：页码 counter(page)</p></li><li><p>要点二：页眉 string-set</p></li></ul>"
        ),
        paras = paras.join("")
    )
}
