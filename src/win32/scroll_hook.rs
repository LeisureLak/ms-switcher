//! 滚轮模式低级鼠标钩子（WH_MOUSE_LL，常驻）+ Raw Input 位移源。
//!
//! 行为（仅在本程序菜单未打开时生效，菜单打开期间整体放行）：
//! - **滚轮模式**：按住配置的触发键（默认侧键1）期间，钩子吞掉
//!   WM_MOUSEMOVE 冻结指针；Y 位移改由 Raw Input 提供（宿主窗口收
//!   `WM_INPUT` 读 `RAWMOUSE.lLastY`），经 [`crate::scroll::WheelAccum`]
//!   攒齿后 `SendInput` 注入 `MOUSEEVENTF_WHEEL`；触发键抬起恢复原样。
//!   不能用钩子的 `pt` 求位移：`pt` 是**光标位置**而非原始位移，吞掉移动
//!   把光标冻结后，系统会生成把位置拉回真实光标的补偿移动——在回调里
//!   表现为反向位移，注入反向滚轮 = 滚动回弹（踩坑 四-17）。Raw Input
//!   由 HID 栈直接产生，与钩子吞移动互不影响，无弹道/钳制/补偿事件。
//! - **触发键录入**：菜单点「触发键」后进入录入态，下一个鼠标键成为触发键
//!   （该次按下被吞掉）；菜单内按**左/右键**视为取消，中键/侧键不与菜单
//!   交互、原地按下照常录入；菜单外任意键均录入。录入结束的那次点击经
//!   `CAPTURE_END_TIME` 时间戳标记，菜单关闭钩子据此豁免、不误关菜单。
//!
//! 纪律（踩坑 三-1 / 性能基线 / 四-16）：
//! - 钩子回调与消息循环同线程（系统经消息泵回调），经 `HOST_PTR` 直接借用
//!   `HostState`，无需锁；回调内只做内存运算与 PostMessage 投递，绝不碰
//!   文件/菜单，也绝不直接 SendInput——注入事件要走同一条低级钩子分发
//!   路径，回调内注入会让线程自锁在 win32k（进程卡死且杀不掉，见踩坑
//!   四-16）；真正的注入在消息循环普通处理上下文里做（`flush_pending_wheel`）。
//! - 注入事件带 LLMHF_INJECTED 标记，回调一律放行（不能自己吞自己）。
//! - 钩子回调超时会被系统静默摘除（LowLevelHooksTimeout），保持回调极短。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging as win;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetWindowRect, HHOOK, LLMHF_INJECTED, MSLLHOOKSTRUCT, PostMessageW,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL, WM_MOUSEMOVE, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
};

use super::host::HostState;
use crate::scroll::{ScrollCfg, TriggerBtn};

/// 录入完成/取消通知（应用私有消息）。wParam = 1 表示已录入新触发键（结果在
/// 静态量里，宿主经 `take_capture_result` 取），0 表示取消。
pub const WM_APP_SCROLL_CHANGED: u32 = win::WM_USER + 5;
/// 冲刷积攒的滚轮注入量（应用私有消息）。
///
/// SendInput 绝不能出现在 LL 钩子回调内：注入的输入要走同一条低级钩子
/// 分发路径，回调内注入会让线程自锁在 win32k——进程卡死且无法结束
/// （踩坑 四-16）。因此回调只累积并投递本消息，由宿主在普通消息处理
/// 上下文里统一注入（`flush_pending_wheel`）。
pub const WM_APP_SCROLL_INJECT: u32 = win::WM_USER + 6;

static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);
static HOST_PTR: AtomicIsize = AtomicIsize::new(0);
/// 本程序菜单打开期间滚轮模式整体放行（侧键等恢复正常语义）。
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// 录入态。
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// 已累积、尚未注入的滚轮量（WHEEL_DELTA=120 的倍数）。
static PENDING_WHEEL: AtomicI32 = AtomicI32::new(0);
/// 队列里是否已有未处理的 `WM_APP_SCROLL_INJECT`（一次泵周期只投一条，防投递风暴）。
static INJECT_POSTED: AtomicBool = AtomicBool::new(false);
/// Raw Input 鼠标源是否已注册（`RIDEV_INPUTSINK` → 宿主窗口后台收 `WM_INPUT`）。
static RAW_REGISTERED: AtomicBool = AtomicBool::new(false);
/// 「结束录入」事件的 `MSLLHOOKSTRUCT.time`（GetMessageTime 口径）。
/// 录入经一次按键按下结束，同一次点击随后还会到达菜单关闭钩子
/// （host.rs）——两个钩子调用次序不定，若本钩子先跑并清掉 CAPTURING，
/// 关闭钩子会把这次「菜单外按下」误判为点击外部而关闭菜单：菜单一关，
/// 刚录入到子菜单预览里的触发键随之丢失。记录时间戳让关闭钩子认出并
/// 豁免这唯一一次事件。
static CAPTURE_END_TIME: AtomicU32 = AtomicU32::new(0);

/// 安装/卸载钩子与 Raw Input 源（功能开关变化时调用）。
pub fn set_enabled(state: &mut HostState, on: bool) {
    if on {
        install(state);
        register_raw_input(state);
    } else {
        // 关功能时复位激活态：钩子卸了就不会再见到触发键抬起
        state.scroll.active = false;
        uninstall();
    }
}

/// 与当前生效规则同步滚轮模式开关。
pub fn sync(state: &mut HostState) {
    let on = effective_scroll(state).is_some();
    set_enabled(state, on);
}

/// 取当前生效规则的滚轮模式配置（仅 enabled 时有效）。
fn effective_scroll(state: &HostState) -> Option<ScrollCfg> {
    state
        .app
        .effective_rule()
        .and_then(|(_, r)| r.scroll.clone())
        .filter(|s| s.enabled)
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
    PENDING_WHEEL.store(0, Ordering::Relaxed);
    INJECT_POSTED.store(false, Ordering::Release);
    unregister_raw_input();
    let h = HOOK_HANDLE.swap(0, Ordering::AcqRel);
    HOST_PTR.store(0, Ordering::Release);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut c_void));
        }
    }
}

/// 注册 Raw Input 鼠标源（Generic Desktop / Mouse），WM_INPUT 投递到宿主窗口。
/// 只在功能开启时注册：常驻注册会让每次物理移动都唤醒消息泵，白白耗电。
fn register_raw_input(state: &HostState) {
    if RAW_REGISTERED.swap(true, Ordering::AcqRel) {
        return;
    }
    let dev = RAWINPUTDEVICE {
        usUsagePage: 0x01, // HID Generic Desktop
        usUsage: 0x02,     // Mouse
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: state.hwnd,
    };
    let ok = unsafe {
        RegisterRawInputDevices(&[dev], std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_ok()
    };
    if !ok {
        // 注册失败（罕见）：回退标志，滚轮模式退化为「只冻指针无滚动」，
        // 不致命；下次开关可重试
        RAW_REGISTERED.store(false, Ordering::Release);
        if state.debug {
            eprintln!("[mss-debug] raw input register failed");
        }
    }
}

fn unregister_raw_input() {
    if !RAW_REGISTERED.swap(false, Ordering::AcqRel) {
        return;
    }
    let dev = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_REMOVE, // hwndTarget 必须为 NULL
        hwndTarget: HWND::default(),
    };
    unsafe {
        let _ = RegisterRawInputDevices(&[dev], std::mem::size_of::<RAWINPUTDEVICE>() as u32);
    }
}

/// 宿主收到 `WM_INPUT` 时调用：滚轮模式激活期间取 Raw Input 相对 Y 位移攒齿。
/// 运行在普通消息处理上下文；与钩子回调一样只累积 + 投递，注入统一走
/// `flush_pending_wheel`。
pub fn on_raw_input(state: &mut HostState, lp: LPARAM) {
    if !state.scroll.active || MENU_OPEN.load(Ordering::Acquire) {
        return;
    }
    let mut raw = RAWINPUT::default();
    let mut cb = std::mem::size_of::<RAWINPUT>() as u32;
    let got = unsafe {
        GetRawInputData(
            HRAWINPUT(lp.0 as *mut c_void),
            RID_INPUT,
            Some((&mut raw as *mut RAWINPUT).cast()),
            &mut cb,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if got == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE.0 {
        return;
    }
    let m = unsafe { raw.data.mouse };
    // 绝对坐标设备（手写板等）的 lLastY 是位置不是位移，跳过；
    // 鼠标/轨迹球都是相对位移
    if m.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 != 0 || m.lLastY == 0 {
        return;
    }
    let scroll = effective_scroll(state).unwrap();
    let px = scroll
        .px_per_notch
        .clamp(crate::scroll::SCROLL_PX_MIN, crate::scroll::SCROLL_PX_MAX) as f32;
    let units = state.scroll.accum.feed(m.lLastY as f32, px);
    if units != 0 {
        if state.debug {
            eprintln!("[mss-debug] scroll inject {units}");
        }
        queue_wheel(state, units);
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

/// 录入态被一次按下事件结束（录入成功或取消）：退出录入态并记录该事件的
/// 时间戳，供菜单关闭钩子豁免同一次点击（见 `CAPTURE_END_TIME`）。
fn finish_capture(event_time: u32) {
    CAPTURE_END_TIME.store(event_time, Ordering::Release);
    cancel_capture();
}

/// 该时间戳的事件是否就是「结束录入」的那次按键。菜单关闭钩子用它豁免
/// 这次点击，与两个钩子的调用次序无关。
pub fn is_capture_end_event(event_time: u32) -> bool {
    CAPTURE_END_TIME.load(Ordering::Acquire) == event_time
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

/// 钩子回调内调用：累积滚轮量并保证队列里有一条冲刷消息。
/// 只做内存运算 + PostMessage，绝不在这里 SendInput（见 `WM_APP_SCROLL_INJECT`）。
fn queue_wheel(state: &HostState, units: i32) {
    PENDING_WHEEL.fetch_add(units, Ordering::Relaxed);
    if !INJECT_POSTED.swap(true, Ordering::AcqRel) {
        let posted = unsafe {
            PostMessageW(Some(state.hwnd), WM_APP_SCROLL_INJECT, WPARAM(0), LPARAM(0)).is_ok()
        };
        if !posted {
            // 投递失败（罕见，如队列满）：复位标志，让下一次累积能重新投递
            INJECT_POSTED.store(false, Ordering::Release);
        }
    }
}

/// 宿主收到 `WM_APP_SCROLL_INJECT` 后调用：把积攒量一次性 SendInput。
/// 运行在普通消息处理上下文（不在钩子回调内），SendInput 在这里是安全的。
pub fn flush_pending_wheel() {
    // 先清标志再取量：期间钩子新累积的量会看到标志已清、重新投递，
    // 不会丢（最坏情况多投一条空消息）。
    INJECT_POSTED.store(false, Ordering::Release);
    let units = PENDING_WHEEL.swap(0, Ordering::AcqRel);
    if units != 0 {
        // 滚轮 delta 是 i16 量级，限幅防多次累积溢出
        inject_wheel(units.clamp(-32760, 32760));
    }
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
                        // 录入：菜单内按左/右键视为取消（用户是想与菜单交互）；
                        // 中键/侧键不与菜单交互，原地按下照常录入——「点录入
                        // →原地按侧键」是最自然的手势，不该被静默取消。
                        // 菜单外按任意键 = 录入。取消也通知宿主刷新。
                        let inside = is_pt_in_menu(&info.pt, state);
                        match (inside, trigger_from(msg, info.mouseData)) {
                            (true, Some(TriggerBtn::Left | TriggerBtn::Right)) | (true, None) => {
                                finish_capture(info.time);
                                if state.debug {
                                    eprintln!("[mss-debug] scroll capture cancelled (inside menu)");
                                }
                                notify_host(state, false, 0);
                                return LRESULT(1);
                            }
                            (_, Some(t)) => {
                                // 一次性：录入成功立即退出录入态（否则后续
                                // 任意按键都会被当成新触发键覆盖掉）
                                finish_capture(info.time);
                                // 结果经消息参数传递（不存静态量，防菜单关闭时被清）
                                notify_host(state, true, t.code());
                                if state.debug {
                                    eprintln!("[mss-debug] scroll capture recorded {:?}", t);
                                }
                                return LRESULT(1);
                            }
                            (false, None) => {}
                        }
                    }
                } else if !MENU_OPEN.load(Ordering::Acquire) {
                    if let Some(scroll) = effective_scroll(state) {
                        let t = scroll.trigger;
                        if matches_down(msg, info.mouseData, t) {
                            // 进入滚轮模式：吞掉触发键按下。位移源是 Raw Input，
                            // 这里不再记基准点（见模块头注释 / 踩坑 四-17）
                            state.scroll.accum.reset();
                            state.scroll.active = true;
                            if state.debug {
                                eprintln!(
                                    "[mss-debug] scroll mode ON (raw input, {} px/notch)",
                                    scroll.px_per_notch
                                );
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
                                // 移动被吞：指针冻结。这里的 pt 不可用作位移
                                // （冻结光标后系统会补偿/钳制，见踩坑 四-17），
                                // Y 位移由 WM_INPUT → on_raw_input 提供。
                                return LRESULT(1);
                            }
                            // 其余按键/滚轮消息照常放行
                        }
                    }
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// 检查屏幕点是否位于主菜单或子菜单窗口内。
fn is_pt_in_menu(pt: &POINT, state: &HostState) -> bool {
    let mut rc = RECT::default();
    for hwnd in [state.menu, state.sub].iter().filter_map(|&h| h) {
        unsafe {
            if GetWindowRect(hwnd, &mut rc).is_ok() {
                if pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom {
                    return true;
                }
            }
        }
    }
    false
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
