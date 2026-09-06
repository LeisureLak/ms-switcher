//! 菜单关闭时的前台焦点归还。
//!
//! 托盘弹窗存续期间前台被菜单窗口占着；菜单销毁后若不做处理，前台会落在
//! 通知区域（任务栏）上，表现为「焦点锁在托盘图标」——用户回到自己原来
//! 的窗口前还得再点一下。
//!
//! 归还策略（菜单销毁前调用，此时本进程仍持有前台、有权移交）：
//! - 当前前台已是普通用户窗口（点击菜单外关闭时系统已激活点击目标）
//!   → 尊重现状，不动；
//! - 前台是我们的窗口 / 任务栏等 Shell 窗口 / 无前台 → 按 Z 序找最上层
//!   「可见、未遮蔽、可激活、非 Shell、非本进程」的顶层窗口，经
//!   AttachThreadInput 把前台交给它（前台在任务栏手里时直接
//!   `SetForegroundWindow` 会被前台锁拒绝，附加输入队列是标准解法，
//!   与 menu.rs 打开菜单时的 force_foreground 同理）；
//! - 找不到归还目标 → 返回 false，调用方退回 NIM_SETFOCUS 旧行为。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::core::BOOL;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GW_OWNER, GetClassNameW, GetForegroundWindow, GetWindow,
    GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
    WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// 不应作为归还目标的 Shell 顶层窗口类名（大小写不敏感）。
const SHELL_CLASSES: &[&str] = &[
    "Shell_TrayWnd",                       // 主任务栏
    "Shell_SecondaryTrayWnd",              // 副屏任务栏
    "NotifyIconOverflowWindow",            // 旧版通知区域溢出浮层
    "TopLevelWindowForOverflowXamlIsland", // Win11 通知区域溢出浮层
    "Progman",                             // 桌面
    "WorkerW",
    "MultitaskingViewFrame",               // 任务视图
    "WindowsDashboard",                    // 小组件面板
    "Xaml_WindowedPopupClass",             // Win11 XAML 弹层（快速设置等）
];

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// `hwnd` 是否适合作为焦点归还目标：可见、非本进程、可激活、非 Shell 窗口。
fn is_user_window(hwnd: HWND, self_pid: u32) -> bool {
    if hwnd.is_invalid() || !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return false;
    }
    let mut pid = 0u32;
    unsafe {
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == self_pid {
        return false; // 自己的宿主/菜单/子菜单
    }
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if ex & WS_EX_NOACTIVATE.0 != 0 {
        return false; // 声明不可激活，给了也白给
    }
    // Alt+Tab 口径：无 WS_EX_APPWINDOW 时，TOOLWINDOW 或有 owner 的顶层窗口
    // 都不是「用户窗口」。任务栏的「开始」按钮（owner=Shell_TrayWnd 的
    // TOOLWINDOW 顶层窗口）等 Shell 部件会被这条挡下。
    if ex & WS_EX_APPWINDOW.0 == 0 {
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let owner = unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default();
        if !owner.is_invalid() {
            return false;
        }
    }
    let name = class_name(hwnd);
    if SHELL_CLASSES.iter().any(|c| name.eq_ignore_ascii_case(c)) {
        return false;
    }
    // 遮蔽窗口（挂起的 UWP 等）：SetForegroundWindow 对它们必失败
    let mut cloaked: u32 = 0;
    let _ = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        )
    };
    cloaked == 0
}

struct ScanCtx {
    self_pid: u32,
    found: HWND,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lp: LPARAM) -> BOOL {
    let ctx = &mut *(lp.0 as *mut ScanCtx);
    if is_user_window(hwnd, ctx.self_pid) {
        ctx.found = hwnd;
        return BOOL(0); // EnumWindows 按 Z 序自上而下，首个命中即最上层用户窗口
    }
    BOOL(1)
}

/// Z 序最靠前的可归还目标（= 菜单弹出前用户最可能在用的窗口）。
fn topmost_user_window(self_pid: u32) -> Option<HWND> {
    let mut ctx = ScanCtx {
        self_pid,
        found: HWND::default(),
    };
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut ScanCtx as isize)) };
    (!ctx.found.is_invalid()).then_some(ctx.found)
}

/// 调试日志：`MSS_DEBUG_FOCUS=1` 时把焦点归还过程追加到 %TEMP%\mss_focus.log
/// （GUI 子系统进程没有 stderr 可用，只能写文件）。
fn dbg_log(msg: &str) {
    if std::env::var("MSS_DEBUG_FOCUS").is_err() {
        return;
    }
    let Ok(t) = std::env::var("TEMP") else { return };
    let p = std::path::Path::new(&t).join("mss_focus.log");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{msg}");
    }
}

/// 菜单关闭时调用：把前台归还给用户窗口。
///
/// 返回 false 表示找不到可归还的目标（调用方可退回 NIM_SETFOCUS 旧行为）。
/// 必须在菜单窗口销毁之前调用——本进程仍持有前台时才有权把前台让出去。
pub fn restore() -> bool {
    let self_pid = unsafe { GetCurrentProcessId() };
    let fg = unsafe { GetForegroundWindow() };
    dbg_log(&format!("restore: fg={:?} cls={:?}", fg, class_name(fg)));
    // 前台已是别人的正常窗口（点击菜单外关闭时系统已激活目标）：不插手
    if is_user_window(fg, self_pid) {
        dbg_log("  fg already a user window, leave");
        return true;
    }
    let Some(target) = topmost_user_window(self_pid) else {
        dbg_log("  no restore target found");
        return false;
    };
    unsafe {
        let cur = GetCurrentThreadId();
        let target_thread = GetWindowThreadProcessId(target, None);
        // 把本线程附加到目标线程的输入队列：共享输入状态后，前台锁把本线程
        // 视为前台线程，SetForegroundWindow 才被放行（实测本进程虽持有前台，
        // 裸调用仍会被拒——从没收到过真实输入的线程不算「前台线程」）。
        let attached = target_thread != 0
            && target_thread != cur
            && AttachThreadInput(cur, target_thread, true).as_bool();
        let ok = SetForegroundWindow(target).as_bool();
        if attached {
            let _ = AttachThreadInput(cur, target_thread, false);
        }
        dbg_log(&format!(
            "  target={:?} cls={:?} attached={attached} ok={ok} fg_after={:?}",
            target,
            class_name(target),
            class_name(GetForegroundWindow())
        ));
        ok
    }
}
