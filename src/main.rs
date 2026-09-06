#![windows_subsystem = "windows"]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mouse_speed_switcher::{app, config, devices, state::AppState, tray};
use windows::core::w;
use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::Foundation::GetLastError;

/// 极简日志：MSS_DEBUG=1 时把 log/warn/error 打到 stderr（诊断 viewport 等内部问题用）。
struct DebugLogger;
static LOG_INIT: std::sync::Once = std::sync::Once::new();

fn main() {
    LOG_INIT.call_once(|| {
        if std::env::var("MSS_DEBUG").is_ok() {
            let _ = log::set_boxed_logger(Box::new(DebugLogger));
            log::set_max_level(log::LevelFilter::Debug);
        }
    });

    // 单实例：已存在则静默退出
    match unsafe { CreateMutexW(None, true, w!("Local\\MouseSpeedSwitcher_Singleton")) } {
        Ok(m) => {
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                return;
            }
            // HANDLE 是 Copy 且无 Drop，绑定保留到 main 结束即可
            let _ = m;
        }
        Err(_) => return,
    }

    // 初始化：读配置、枚举已插入设备并应用规则
    let app_state = Arc::new(Mutex::new(Some(AppState::new(config::load()))));
    let dirty = Arc::new(AtomicBool::new(false));
    let (tip_tx, tip_rx) = std::sync::mpsc::channel::<String>();

    // 托盘图标
    let (cur, rule) = {
        let guard = app_state.lock().unwrap();
        (
            speed_get(),
            guard
                .as_ref()
                .and_then(|s| s.effective_rule())
                .map(|(d, sp)| (d.name.clone(), sp)),
        )
    };
    let tip = app::tip_text(cur, rule.as_ref().map(|(n, s)| (n.as_str(), *s)));
    let tray = match tray::TrayHandle::new(&tip) {
        Some(t) => t,
        None => return,
    };

    // 托盘事件线程：点击（左/右键抬起）→ 置位 TRAY_CLICK
    std::thread::spawn(|| {
        let rx = tray_icon::TrayIconEvent::receiver();
        while let Ok(ev) = rx.recv() {
            if let tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left | tray_icon::MouseButton::Right,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = ev
            {
                app::TRAY_CLICK.store(true, Ordering::Relaxed);
            }
        }
    });

    // 设备轮询线程：替代原 WM_DEVICECHANGE 去抖 + 30s 兜底轮询，
    // 直接周期性枚举设备并应用差异，同时同步托盘 tooltip。
    {
        let app_state = app_state.clone();
        let dirty = dirty.clone();
        let tip_tx = tip_tx.clone();
        std::thread::spawn(move || {
            let mut last_ids: Vec<String> = Vec::new();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let mice = devices::enumerate_mice();
                let ids: Vec<String> =
                    mice.iter().map(|d| d.instance_id.clone()).collect();
                {
                    let mut guard = app_state.lock().unwrap();
                    if let Some(st) = guard.as_mut() {
                        st.apply_diff(&mice);
                    }
                }
                if ids != last_ids {
                    last_ids = ids;
                    dirty.store(true, Ordering::Relaxed);
                }
                let (cur, rule) = {
                    let guard = app_state.lock().unwrap();
                    (
                        speed_get(),
                        guard
                            .as_ref()
                            .and_then(|s| s.effective_rule())
                            .map(|(d, sp)| (d.name.clone(), sp)),
                    )
                };
                let _ = tip_tx.send(app::tip_text(
                    cur,
                    rule.as_ref().map(|(n, s)| (n.as_str(), *s)),
                ));
            }
        });
    }

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_active(false)
            .with_taskbar(false)
            .with_inner_size([1.0, 1.0])
            .with_position([-32000.0, -32000.0]),
        ..Default::default()
    };
    let r = eframe::run_native(
        "MouseSpeedSwitcher",
        opts,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, app_state, tray, dirty, tip_rx)))),
    );
    let _ = r;
}

fn speed_get() -> u32 {
    mouse_speed_switcher::speed::get()
}

impl log::Log for DebugLogger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record) {
        eprintln!("[log-{}] {}", record.level(), record.args());
    }
    fn flush(&self) {}
}
