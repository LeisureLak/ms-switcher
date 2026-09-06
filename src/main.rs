#![windows_subsystem = "windows"]

use std::ffi::c_void;
use std::sync::Mutex;

use mouse_speed_switcher::{
    autostart, config, devices, popup_menu, speed, state::AppState, tray, tray::Tray,
};
use windows::core::{w, BOOL, GUID, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

use tray::WM_TRAYICON;

// ── 定时器 ──────────────────────────────────────────────
const TIMER_RESCAN: usize = 1;
const TIMER_POLL: usize = 2;
/// 插拔事件后的去抖延迟（ms）。
const RESCAN_DELAY_MS: u32 = 400;
/// 兜底轮询间隔（ms）：即使设备通知丢失也能自愈。
const POLL_INTERVAL_MS: u32 = 30_000;

// ── 菜单命令 ID ─────────────────────────────────────────
const IDM_RELOAD: u32 = 900;
const IDM_AUTOSTART: u32 = 901;
const IDM_EXIT: u32 = 902;
/// 设备命令基址：每个设备占 3 个命令槽。
const IDM_DEV_BASE: u32 = 100;
const IDM_DEV_MAX: u32 = 100 + 3 * 1000;
const SLOT_SET_RULE: u32 = 0;
const SLOT_DEL_RULE: u32 = 1;
const SLOT_REAPPLY: u32 = 2;

/// GUID_DEVINTERFACE_MOUSE = {378de44c-56ef-11d1-bc8c-00a0c91405dd}
const GUID_DEVINTERFACE_MOUSE: GUID = GUID {
    data1: 0x378de44c,
    data2: 0x56ef,
    data3: 0x11d1,
    data4: [0xbc, 0x8c, 0x00, 0xa0, 0xc9, 0x14, 0x05, 0xdd],
};

static APP_STATE: Mutex<Option<AppState>> = Mutex::new(None);
static TRAY: Mutex<Option<Tray>> = Mutex::new(None);
/// 主窗口句柄（供菜单命令回调取用；HWND 裸指针跨 static 保存）。
static MENU_HOST: Mutex<Option<RawHwnd>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct RawHwnd(isize);

impl RawHwnd {
    fn hwnd(&self) -> HWND {
        HWND(self.0 as *mut std::ffi::c_void)
    }
}

fn main() {
    // DPI 感知：按真实 DPI 渲染，避免高分屏被系统位图拉伸导致模糊
    let dpi_res = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("C:\\Users\\lak\\AppData\\Local\\Temp\\dpi_trace.log")
        {
            let _ = writeln!(f, "SetProcessDpiAwarenessContext(PMv2): {:?}", dpi_res);
        }
    }
    let _ = dpi_res;

    // 单实例：已存在则静默退出
    match unsafe { CreateMutexW(None, true, w!("Local\\MouseSpeedSwitcher_Singleton")) } {
        Ok(m) => {
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                return;
            }
            // HANDLE 是 Copy 且无 Drop，绑定保留到 main 结束即可，无需显式关闭
            let _ = m;
        }
        Err(_) => return,
    }

    let hinst: HINSTANCE = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h.into(),
        Err(_) => return,
    };
    let icon = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();

    let class = w!("MouseSpeedSwitcherWndClass");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: icon,
        hCursor: HCURSOR::default(),
        hbrBackground: HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: class,
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        return;
    }

    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("MouseSpeedSwitcher"),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )
    } {
        Ok(h) => h,
        Err(_) => return,
    };
    // 窗口保持隐藏：仅用于接收设备通知和托盘消息
    *MENU_HOST.lock().unwrap() = Some(RawHwnd(hwnd.0 as isize));

    // 托盘图标
    if let Some(t) = Tray::add(hwnd, icon) {
        *TRAY.lock().unwrap() = Some(t);
    }

    // 初始化：读配置、枚举已插入设备并应用规则
    {
        let cfg = config::load();
        *APP_STATE.lock().unwrap() = Some(AppState::new(cfg));
        update_tip(hwnd);
    }

    let mut msg = MSG::default();
    loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r == BOOL(0) || r == BOOL(-1) {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            register_device_notification(hwnd);
            unsafe {
                SetTimer(Some(hwnd), TIMER_POLL, POLL_INTERVAL_MS, None);
            }
            LRESULT(0)
        }
        WM_DEVICECHANGE => {
            // 任何设备变化：去抖后重扫
            unsafe {
                SetTimer(Some(hwnd), TIMER_RESCAN, RESCAN_DELAY_MS, None);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_RESCAN {
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_RESCAN);
                }
                rescan_and_update_tip(hwnd);
            } else if wparam.0 == TIMER_POLL {
                rescan_and_update_tip(hwnd);
            }
            LRESULT(0)
        }
        WM_TRAYICON => {
            if tray::is_click(msg, lparam.0) {
                show_tray_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_menu_command(hwnd, loword(wparam));
            LRESULT(0)
        }
        // 指针速度变化（滑动条或系统设置）后同步托盘提示
        WM_SETTINGCHANGE => {
            update_tip(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(t) = TRAY.lock().unwrap().as_mut() {
                t.remove();
            }
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 注册鼠标设备接口通知：插入/拔出时收到 WM_DEVICECHANGE。
fn register_device_notification(hwnd: HWND) {
    let mut filter: DEV_BROADCAST_DEVICEINTERFACE_W = unsafe { std::mem::zeroed() };
    filter.dbcc_size = std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32;
    filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE.0;
    filter.dbcc_classguid = GUID_DEVINTERFACE_MOUSE;
    let _ = unsafe {
        RegisterDeviceNotificationW(
            HANDLE(hwnd.0 as *mut c_void),
            &filter as *const _ as *const c_void,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    };
}

fn rescan_and_update_tip(hwnd: HWND) {
    if let Some(st) = APP_STATE.lock().unwrap().as_mut() {
        st.rescan();
    }
    update_tip(hwnd);
}

fn update_tip(_hwnd: HWND) {
    let cur = speed::get();
    let rule = APP_STATE
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|s| s.effective_rule())
        .map(|(d, sp)| format!(" · 生效规则: {} (速度 {})", d.name, sp));
    let tip = match rule {
        Some(r) => format!("鼠标灵敏度切换 - 当前指针速度: {}{}", cur, r),
        None => format!("鼠标灵敏度切换 - 当前指针速度: {}", cur),
    };
    if let Some(t) = TRAY.lock().unwrap().as_mut() {
        t.set_tip(&tip);
    }
}

// ── 托盘菜单（自绘弹窗，内嵌滑动条）────────────────────

fn show_tray_menu(hwnd: HWND) {
    // 组装设备子菜单数据（保留旧菜单逻辑：规则标记 + 生效勾选 + 多规则说明行）
    let mice = devices::enumerate_mice();
    let (effective, rule_count) = {
        let guard = APP_STATE.lock().unwrap();
        let effective: Option<(String, String)> = guard
            .as_ref()
            .and_then(|s| s.effective_rule())
            .and_then(|(d, _)| {
                let v = d.vid.clone()?;
                let p = d.pid.clone()?;
                Some((v, p))
            });
        let count = guard.as_ref().map(|s| s.active_rule_count()).unwrap_or(0);
        (effective, count)
    };
    let devs: Vec<popup_menu::DevMenu> = mice
        .iter()
        .enumerate()
        .map(|(i, dev)| {
            let rule_speed = {
                let guard = APP_STATE.lock().unwrap();
                dev.vid
                    .as_deref()
                    .zip(dev.pid.as_deref())
                    .and_then(|(v, p)| {
                        guard
                            .as_ref()
                            .and_then(|s| s.cfg.rule_for(v, p))
                            .map(|r| r.speed)
                    })
            };
            // 生效中：该设备的 VID/PID 与最后插入的规则设备一致（同型号共用规则）
            let is_effective = rule_speed.is_some()
                && effective
                    .as_ref()
                    .map(|(ev, ep)| {
                        ev == dev.vid.as_deref().unwrap_or("")
                            && ep == dev.pid.as_deref().unwrap_or("")
                    })
                    .unwrap_or(false);
            let base = IDM_DEV_BASE + (i as u32) * 3;
            popup_menu::DevMenu {
                name: dev.name.clone(),
                vid: dev.vid.clone(),
                pid: dev.pid.clone(),
                rule_speed,
                is_effective,
                cmds: [base + SLOT_SET_RULE, base + SLOT_DEL_RULE, base + SLOT_REAPPLY],
            }
        })
        .collect();
    // 多规则并存：顶部一行说明当前生效者（沿用旧菜单行为）
    let effective_text = if rule_count >= 2 {
        APP_STATE.lock().unwrap().as_ref().and_then(|s| {
            s.effective_rule()
                .map(|(d, sp)| format!("生效规则: {} (速度 {})", d.name, sp))
        })
    } else {
        None
    };
    let header = popup_menu::MenuHeader {
        effective_text,
        autostart_on: autostart::is_enabled(),
    };
    popup_menu::show(hwnd, devs, header, run_menu_command);
}

/// 菜单项命令处理（自绘菜单与 WM_COMMAND 共用）。
fn run_menu_command(cmd: u32) {
    let h = MENU_HOST.lock().unwrap().map(|h| h.hwnd());
    if let Some(hwnd) = h {
        handle_menu_command(hwnd, cmd);
    }
}

fn handle_menu_command(hwnd: HWND, cmd: u32) {
    match cmd {
        IDM_RELOAD => {
            let cfg = config::load();
            *APP_STATE.lock().unwrap() = Some(AppState::new(cfg));
            update_tip(hwnd);
        }
        IDM_AUTOSTART => {
            if autostart::is_enabled() {
                autostart::disable();
            } else {
                autostart::enable();
            }
        }
        IDM_EXIT => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        c if c >= IDM_DEV_BASE && c < IDM_DEV_MAX => {
            let idx = ((c - IDM_DEV_BASE) / 3) as usize;
            let slot = (c - IDM_DEV_BASE) % 3;
            let mice = devices::enumerate_mice();
            if let Some(dev) = mice.get(idx) {
                let (Some(vid), Some(pid)) = (&dev.vid, &dev.pid) else {
                    return;
                };
                let mut guard = APP_STATE.lock().unwrap();
                let st = guard.as_mut().unwrap();
                match slot {
                    SLOT_SET_RULE => {
                        let cur = speed::get();
                        st.cfg.set_rule(vid, pid, cur, Some(dev.name.clone()));
                        let _ = config::save(&st.cfg);
                        st.reapply();
                    }
                    SLOT_DEL_RULE => {
                        st.cfg.remove_rule(vid, pid);
                        let _ = config::save(&st.cfg);
                        st.reapply();
                    }
                    SLOT_REAPPLY => {
                        st.reapply();
                    }
                    _ => {}
                }
            }
            update_tip(hwnd);
        }
        _ => {}
    }
}

// ── 工具函数 ────────────────────────────────────────────

fn loword(v: WPARAM) -> u32 {
    (v.0 & 0xFFFF) as u32
}
