//! 滚轮模式低级鼠标钩子（WH_MOUSE_LL，常驻）。
//!
//! 行为（仅在本程序菜单未打开时生效，菜单打开期间整体放行）：
//! - **滚轮模式**：按住配置的触发键（默认侧键1）期间，WM_MOUSEMOVE 被吞掉，
//!   Y 位移经 [`crate::scroll::WheelAccum`] 攒齿后 `SendInput` 注入
//!   `MOUSEEVENTF_WHEEL`；触发键抬起恢复原样。
//! - **触发键录入**：菜单点「触发键」后进入录入态，下一个落在**菜单之外**的
//!   鼠标键成为触发键（该次按下被吞掉）；点菜单内任意处取消录入。
//!
//! 纪律（踩坑 三-1 / 性能基线）：
//! - 钩子回调与消息循环同线程（系统经消息泵回调），经 `HOST_PTR` 直接借用
//!   `HostState`，无需锁；回调内只做内存运算与 SendInput，绝不碰文件/菜单。
//! - 注入事件带 LLMHF_INJECTED 标记，回调一律放行（不能自己吞自己）。
//! - 钩子回调超时会被系统静默摘除（LowLevelHooksTimeout），保持回调极短。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging as win;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, MSLLHOOKSTRUCT, PostMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, LLMHF_INJECTED, WH_MOUSE_LL,
    WM_MOUSEMOVE, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use super::host::HostState;
use crate::scroll::TriggerBtn;

/// 录入完成/取消通知（应用私有消息）。wParam = 1 表示已录入新触发键（结果在
/// 静态量里，宿主经 `take_capture_result` 取），0 表示取消。
pub const WM_APP_SCROLL_CHANGED: u32 = win::WM_USER + 5;

static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);
static HOST_PTR: AtomicIsize = AtomicIsize::new(0);
/// 本程序菜单打开期间滚轮模式整体放行（侧键等恢复正常语义）。
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// 录入态。
static CAPTURING: AtomicBool = AtomicBool::new(false);

/// 安装/卸载钩子（功能开关变化时调用）。
pub fn set_enabled(state: &mut HostState, on: bool) {
    if on {
        install(state);
    } else {
        uninstall();
    }
}

fn install(state: &mut HostState) {
    if HOOK_HANDLE.load(Ordering::Acquire) != 0 {
        return;
    }
    match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), None, 0) } {
        Ok(h) => {
            HOOK_HANDLE.store(h.0 as isize, Ordering::Release);
            HOST_PTR.store(state as *mut HostState as isize, Ordering::Release);
            if state.debug {
                eprintln!("[mss-debug] scroll hook installed");
            }
        }
        Err(e) => {
            // 钩子装不上（罕见）：功能静默不可用，不致命
            if state.debug {
                eprintln!("[mss-debug] scroll hook install failed: {e}");
            }
        }
    }
}

fn uninstall() {
    CAPTURING.store(false, Ordering::Relaxed);
    let h = HOOK_HANDLE.swap(0, Ordering::AcqRel);
    HOST_PTR.store(0, Ordering::Release);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut c_void));
        }
    }
}

/// 进入触发键录入态（菜单「触发键」行点击）。
pub fn arm_capture(state: &mut HostState) {
    // 录入依赖钩子；功能未开时也要临时装上
    install(state);
    CAPTURING.store(true, Ordering::Release);
    if state.debug {
        eprintln!("[mss-debug] scroll capture armed");
    }
}

/// 取消录入态（静默；宿主侧自行刷新模型）。
pub fn cancel_capture() {
    CAPTURING.store(false, Ordering::Release);
}

/// 当前是否在录入态（菜单绘制/关菜单钩子判断用）。
pub fn is_capturing() -> bool {
    CAPTURING.load(Ordering::Acquire)
}

/// 菜单开合通知（打开期间滚轮模式放行、关闭时顺带取消录入）。
pub fn set_menu_open(open: bool) {
    MENU_OPEN.store(open, Ordering::Release);
    if !open {
        cancel_capture();
    }
}

/// WM_*BUTTONDOWN 消息值。
fn is_button_down_msg(msg: u32) -> bool {
    matches!(msg, 0x0201 | 0x0204 | 0x0207 | 0x020B) // L/R/M/XBUTTONDOWN
}

/// 事件是否为触发键的按下（X 键看 mouseData 高字的键号）。
fn matches_down(msg: u32, mouse_data: u32, t: TriggerBtn) -> bool {
    match t {
        TriggerBtn::Left => msg == 0x0201,
        TriggerBtn::Right => msg == 0x0204,
        TriggerBtn::Middle => msg == 0x0207,
        TriggerBtn::X1 => msg == WM_XBUTTONDOWN && (mouse_data >> 16) == 1,
        TriggerBtn::X2 => msg == WM_XBUTTONDOWN && (mouse_data >> 16) == 2,
    }
}

/// 事件是否为触发键的抬起。
fn matches_up(msg: u32, mouse_data: u32, t: TriggerBtn) -> bool {
    match t {
        TriggerBtn::Left => msg == 0x0202,
        TriggerBtn::Right => msg == 0x0205,
        TriggerBtn::Middle => msg == 0x0208,
        TriggerBtn::X1 => msg == WM_XBUTTONUP && (mouse_data >> 16) == 1,
        TriggerBtn::X2 => msg == WM_XBUTTONUP && (mouse_data >> 16) == 2,
    }
}

/// 录入态按下事件的按键归类。
fn trigger_from(msg: u32, mouse_data: u32) -> Option<TriggerBtn> {
    Some(match msg {
        0x0201 => TriggerBtn::Left,
        0x0204 => TriggerBtn::Right,
        0x0207 => TriggerBtn::Middle,
        WM_XBUTTONDOWN => match mouse_data >> 16 {
            1 => TriggerBtn::X1,
            2 => TriggerBtn::X2,
            _ => return None,
        },
        _ => return None,
    })
}

/// 注入一齿（或数齿）纵向滚轮。
fn inject_wheel(delta: i32) {
    let inp = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[inp], std::mem::size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let ptr = HOST_PTR.load(Ordering::Acquire);
        if ptr != 0 {
            let state = &mut *(ptr as *mut HostState);
            let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
            let msg = wp.0 as u32;
            // 诊断：录入态打印所有非移动事件（含注入事件），用于定位按键未识别
            if state.debug
                && CAPTURING.load(Ordering::Acquire)
                && (msg != WM_MOUSEMOVE || info.flags != 0)
            {
                eprintln!(
                    "[mss-debug] capture evt msg={:#06x} data={:#x} flags={:#x}",
                    msg, info.mouseData, info.flags
                );
            }
            // 注入事件（含我们自己发的滚轮）一律放行
            if info.flags & LLMHF_INJECTED == 0 {
                if CAPTURING.load(Ordering::Acquire) {
                    if is_button_down_msg(msg) {
                        // 任何鼠标键都直接录入（无论光标在哪，按下即吞掉，
                        // 不交给菜单）；取消只有 Esc 一种方式，零歧义
                        if let Some(t) = trigger_from(msg, info.mouseData) {
                            // 一次性：录入成功立即退出录入态（否则后续
                            // 任意按键都会被当成新触发键覆盖掉）
                            cancel_capture();
                            // 结果经消息参数传递（不存静态量，防菜单关闭时被清）
                            notify_host(state, true, t.code());
                            if state.debug {
                                eprintln!("[mss-debug] scroll capture recorded {:?}", t);
                            }
                            return LRESULT(1);
                        }
                    }
                } else if !MENU_OPEN.load(Ordering::Acquire)
                    && state.app.cfg.scroll.enabled
                {
                    let t = state.app.cfg.scroll.trigger;
                    if matches_down(msg, info.mouseData, t) {
                        // 进入滚轮模式：吞掉触发键按下，记录基准 Y
                        state.scroll.accum.reset();
                        state.scroll.last_y = info.pt.y;
                        state.scroll.active = true;
                        if state.debug {
                            eprintln!("[mss-debug] scroll mode ON (dy accum per {} px)", state.app.cfg.scroll.px_per_notch);
                        }
                        return LRESULT(1);
                    }
                    if state.scroll.active {
                        if matches_up(msg, info.mouseData, t) {
                            state.scroll.active = false;
                            if state.debug {
                                eprintln!("[mss-debug] scroll mode OFF");
                            }
                            return LRESULT(1);
                        }
                        if msg == WM_MOUSEMOVE {
                            let dy = info.pt.y - state.scroll.last_y;
                            state.scroll.last_y = info.pt.y;
                            if dy != 0 {
                                let px = state
                                    .app
                                    .cfg
                                    .scroll
                                    .px_per_notch
                                    .clamp(crate::scroll::SCROLL_PX_MIN, crate::scroll::SCROLL_PX_MAX)
                                    as f32;
                                let units = state.scroll.accum.feed(dy as f32, px);
                                if units != 0 {
                                    if state.debug {
                                        eprintln!("[mss-debug] scroll inject {units}");
                                    }
                                    inject_wheel(units);
                                }
                            }
                            // 移动被吞：指针不动
                            return LRESULT(1);
                        }
                        // 其余按键/滚轮消息照常放行
                    }
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// 录入结束 → 通知宿主线程收尾（落盘 / 刷新菜单模型）。
/// `recorded` = true 时 `code` 为新触发键的 [`TriggerBtn::code`]；false = 取消。
fn notify_host(state: &HostState, recorded: bool, code: u32) {
    unsafe {
        let _ = PostMessageW(
            Some(state.hwnd),
            WM_APP_SCROLL_CHANGED,
            WPARAM(recorded as usize),
            LPARAM(code as isize),
        );
    }
}
