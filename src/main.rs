#![windows_subsystem = "windows"]

//! MouseSpeedSwitcher 入口：现代原生 Win32 架构。
//!
//! - 隐藏宿主窗口 + 阻塞式 `GetMessageW` 事件循环（空闲 0 轮询、0 重绘）。
//! - 托盘：`Shell_NotifyIconW`；设备监听：`RegisterDeviceNotificationW` +
//!   `WM_DEVICECHANGE`（350ms 防抖）；系统速度变化：`WM_SETTINGCHANGE`。
//! - 单线程状态所有权：`HostState` 由 `Box` 唯一持有，窗口过程只借用
//!   （见 win32/host.rs 所有权模型与踩坑规避）。

use mouse_speed_switcher::win32::{self, host, tray};
use mouse_speed_switcher::{config, menu_model, state::AppState, speed};
use windows::core::w;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() {
    // Per-Monitor V2：必须先于任何 HWND 创建
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // 单实例：已存在则静默退出
    match unsafe { CreateMutexW(None, true, w!("Local\\MouseSpeedSwitcher_Singleton")) } {
        Ok(m) => {
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                return;
            }
            let _ = m; // HANDLE 保留到 main 结束
        }
        Err(_) => return,
    }

    // 读配置、枚举已插入设备并应用规则
    let app = AppState::new(config::load());

    // 宿主状态：Box 唯一持有（窗口过程只借用，见 host.rs 所有权模型）
    let mut state = host::HostState::new(app);
    state.hwnd = match host::create_host_window(&mut state) {
        Ok(h) => h,
        Err(_) => return,
    };

    // 鼠标接口设备通知（RAII）
    state.dev_notify = win32::device_notify::MouseDevNotify::new(state.hwnd);

    // 初始 tooltip 与托盘图标
    let rule = menu_model::effective_name(&state.app);
    let tip = menu_model::tip_text(
        speed::get(),
        speed::get_wheel(),
        rule.as_ref().map(|(n, s)| (n.as_str(), *s)),
    );
    state.tray = tray::Tray::new(state.hwnd, &tip);
    if state.tray.is_none() {
        return;
    }

    // 滚轮模式（轨迹球特化）：配置开启时装载 WH_MOUSE_LL 常驻钩子
    let scroll_enabled = state.app.cfg.scroll.enabled;
    win32::scroll_hook::set_enabled(&mut state, scroll_enabled);

    // MSS_DEBUG_MENU=1：启动即弹出菜单（自动化冒烟测试用）
    if std::env::var("MSS_DEBUG_MENU").is_ok() {
        debug_open_menu(&state);
    }

    let _code = host::message_loop();
    // state（Box）在此 drop：托盘 Drop 内部 NIM_DELETE + DestroyIcon，
    // 设备通知 Drop 反注册
}

/// MSS_DEBUG_MENU=1：模拟一次托盘左键抬起（自动化冒烟测试用）。
fn debug_open_menu(state: &host::HostState) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_LBUTTONUP};
    let lp = (WM_LBUTTONUP as isize & 0xFFFF) as isize;
    unsafe {
        let _ = PostMessageW(Some(state.hwnd), tray::WM_APP_TRAY, WPARAM(1), LPARAM(lp));
    }
}
