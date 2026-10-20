// 任何构建都不显示控制台窗口（调试输出仍会写入已附加的终端句柄，
// 从终端启动 `tauri dev` 时依然可见日志）
#![windows_subsystem = "windows"]

fn main() {
    writer_lib::run()
}
