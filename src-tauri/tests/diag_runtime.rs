//! 诊断：tauri::async_runtime 的 tokio 运行时是否启用时间驱动器
//! （tokio::time::timeout 在无时间驱动的运行时上会 panic——若 panic，
//! run_chat_job 内的空闲看门狗会静默杀死整个 AI 作业）

#[test]
fn tauri_rt_time_driver() {
    let h = tauri::async_runtime::spawn(async {
        tokio::time::timeout(std::time::Duration::from_millis(80), futures_util::future::pending::<()>()).await
    });
    let r = tauri::async_runtime::block_on(h);
    match r {
        Ok(Err(_elapsed)) => println!("TIME_DRIVER_OK: timeout 正常触发"),
        Ok(Ok(())) => println!("UNEXPECTED: pending 提前完成"),
        Err(join_err) => println!("TIME_DRIVER_MISSING: 任务 panic（{join_err}）——tokio::time 在此运行时不可用"),
    }
}
