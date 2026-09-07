//! 滚轮模式低级鼠标/键盘钩子（WH_MOUSE_LL + WH_KEYBOARD_LL）+ Raw Input 位移源。
//!
//! 行为（仅在本程序菜单未打开时生效，菜单打开期间整体放行）：
//! - **鼠标触发滚轮模式**：按住配置的鼠标触发键（默认侧键1）期间，钩子吞掉
//!   WM_MOUSEMOVE 冻结指针；Y 位移改由 Raw Input 提供（宿主窗口收
//!   `WM_INPUT` 读 `RAWMOUSE.lLastY`），经 [`crate::scroll::WheelAccum`]
//!   攒齿后 `SendInput` 注入 `MOUSEEVENTF_WHEEL`；触发键抬起恢复原样。
//!   不能用钩子的 `pt` 求位移：`pt` 是**光标位置**而非原始位移，吞掉移动
//!   把光标冻结后，系统会生成把位置拉回真实光标的补偿移动——在回调里
//!   表现为反向位移，注入反向滚轮 = 滚动回弹（踩坑 四-17）。Raw Input
//!   由 HID 栈直接产生，与钩子吞移动互不影响，无弹道/钳制/补偿事件。
//! - **键盘触发滚轮模式**：规则可配置一个单键（如 Alt）。单独按住该键时
//!   进入滚轮模式；按住期间一旦又按下其它任意键（如 Alt+Tab 的 Tab），
//!   立即取消滚轮模式，避免影响系统/应用组合快捷键。开关键松开也取消。
//!   键盘钩子只观察不灭活键盘事件。
//! - **触发键录入**：菜单点「触发键」后进入鼠标触发键录入态，下一个鼠标键
//!   成为触发键（该次按下被吞掉）；点「键盘触发键」后进入键盘录入态，按下单键
//!   即记录。菜单内按**左/右键**视为取消鼠标录入。录入结束经
//!   `CAPTURE_END_TIME` 时间戳让菜单关闭钩子豁免，不误关菜单。
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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU64, AtomicU32, Ordering};

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
    CallNextHookEx, GetWindowRect, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED,
    MSLLHOOKSTRUCT, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_MOUSEMOVE, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
};

use super::host::HostState;
use crate::scroll::{KbTrigger, ScrollCfg, TriggerBtn};

/// 鼠标触发键录入完成/取消通知（应用私有消息）。wParam = 1 表示已录入
/// 新触发键（结果在静态量里，宿主经 `take_capture_result` 取），0 表示取消。
pub const WM_APP_SCROLL_CHANGED: u32 = win::WM_USER + 5;
/// 冲刷积攒的滚轮注入量（应用私有消息）。
///
/// SendInput 绝不能出现在 LL 钩子回调内：注入的输入要走同一条低级钩子
/// 分发路径，回调内注入会让线程自锁在 win32k——进程卡死且无法结束
/// （踩坑 四-16）。因此回调只累积并投递本消息，由宿主在普通消息处理
/// 上下文里统一注入（`flush_pending_wheel`）。
pub const WM_APP_SCROLL_INJECT: u32 = win::WM_USER + 6;
/// 键盘触发键录入完成/取消通知（应用私有消息）。wParam = 1 表示已录入，
/// lParam 低 32 位为归一化虚拟键码；0 表示取消。
pub const WM_APP_KB_SCROLL_CHANGED: u32 = win::WM_USER + 7;

static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);
static KB_HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);
static HOST_PTR: AtomicIsize = AtomicIsize::new(0);
/// 本程序菜单打开期间滚轮模式整体放行（侧键等恢复正常语义）。
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// 鼠标触发键录入态。
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// 键盘触发键录入态。
static KB_CAPTURING: AtomicBool = AtomicBool::new(false);
/// 键盘录入结果（归一化虚拟键码）。
static KB_CAPTURE_VK: AtomicU32 = AtomicU32::new(0);
/// 已累积、尚未注入的滚轮量（WHEEL_DELTA=120 的倍数）。
static PENDING_WHEEL: AtomicI32 = AtomicI32::new(0);
/// 队列里是否已有未处理的 `WM_APP_SCROLL_INJECT`（一次泵周期只投一条，防投递风暴）。
static INJECT_POSTED: AtomicBool = AtomicBool::new(false);
/// Raw Input 鼠标源是否已注册（`RIDEV_INPUTSINK` → 宿主窗口后台收 `WM_INPUT`）。
static RAW_REGISTERED: AtomicBool = AtomicBool::new(false);
/// 「结束鼠标/键盘录入」事件的时间戳（GetMessageTime 口径）。
/// 录入经一次按下结束，同一次点击/按键随后还会到达菜单关闭钩子
/// （host.rs）——两个钩子调用次序不定，若本钩子先跑并清掉 CAPTURING，
/// 关闭钩子会把这次「菜单外按下」误判为点击外部而关闭菜单：菜单一关，
/// 刚录入到子菜单预览里的触发键随之丢失。记录时间戳让关闭钩子认出并
/// 豁免这唯一一次事件。
static CAPTURE_END_TIME: AtomicU32 = AtomicU32::new(0);
/// 当前被按下的键盘键位图（按归一化虚拟键码，256 位 = 4 × u64）。
/// 用于判断「键盘开关键是否单独按住」，以及检测组合键时取消滚轮模式。
static KEYS_DOWN: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// 安装/卸载鼠标钩子与 Raw Input 源（功能开关变化时调用）。
pub fn set_enabled(state: &mut HostState, on: bool) {
    if on {
        install(state);
        register_raw_input(state);
    } else {
        // 关功能时复位激活态：钩子卸了就不会再见到触发键抬起
        state.scroll.active = false;
        state.scroll.mouse_held = false;
        state.scroll.kb_held = false;
        state.scroll.sync_active();
        uninstall();
    }
}

/// 与当前生效规则同步滚轮模式开关（鼠标 + 键盘钩子 + Raw Input）。
pub fn sync(state: &mut HostState) {
    if let Some(scroll) = effective_scroll(state) {
        state.scroll.enabled = true;
        state.scroll.trigger = scroll.trigger;
        state.scroll.kb_trigger = scroll.kb_trigger;
        state.scroll.px_per_notch = scroll
            .px_per_notch
            .clamp(crate::scroll::SCROLL_PX_MIN, crate::scroll::SCROLL_PX_MAX);
        set_enabled(state, true);
        sync_kb_hook(state);
        // 同步后按当前键盘状态重算 active（例如规则刚切到带键盘触发，
        // 用户正按住 Alt）
        update_kb_active(state);
    } else {
        state.scroll.enabled = false;
        state.scroll.trigger = TriggerBtn::X1;
        state.scroll.kb_trigger = None;
        state.scroll.px_per_notch = crate::scroll::SCROLL_PX_DEFAULT;
        set_enabled(state, false);
        sync_kb_hook(state);
    }
}

/// 取当前生效规则的滚轮模式配置（仅 enabled 时有效）。
fn effective_scroll(state: &HostState) -> Option<ScrollCfg> {
    state
        .app
        .effective_rule()
        .and_then(|(_, r)| r.scroll.clone())
        .filter(|s| s.enabled)
}

/// 根据当前生效规则是否需要键盘钩子，安装/卸载 WH_KEYBOARD_LL。
/// 录制键盘触发键时也需要临时装上。
pub fn sync_kb_hook(state: &mut HostState) {
    let need = effective_scroll(state).and_then(|s| s.kb_trigger).is_some()
        || KB_CAPTURING.load(Ordering::Acquire);
    if need {
        install_kb(state);
    } else {
        uninstall_kb();
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

fn install_kb(state: &mut HostState) {
    if KB_HOOK_HANDLE.load(Ordering::Acquire) != 0 {
        return;
    }
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0) } {
        Ok(h) => {
            KB_HOOK_HANDLE.store(h.0 as isize, Ordering::Release);
            HOST_PTR.store(state as *mut HostState as isize, Ordering::Release);
            if state.debug {
                eprintln!("[mss-debug] keyboard hook installed");
            }
        }
        Err(e) => {
            if state.debug {
                eprintln!("[mss-debug] keyboard hook install failed: {e}");
            }
        }
    }
}

fn uninstall() {
    CAPTURING.store(false, Ordering::Relaxed);
    KB_CAPTURING.store(false, Ordering::Relaxed);
    KB_CAPTURE_VK.store(0, Ordering::Relaxed);
    PENDING_WHEEL.store(0, Ordering::Relaxed);
    INJECT_POSTED.store(false, Ordering::Release);
    unregister_raw_input();
    let h = HOOK_HANDLE.swap(0, Ordering::AcqRel);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut c_void));
        }
    }
    uninstall_kb();
    HOST_PTR.store(0, Ordering::Release);
    // 清键盘位图，避免卸载前残留的键状态被误用
    for k in &KEYS_DOWN {
        k.store(0, Ordering::Relaxed);
    }
}

fn uninstall_kb() {
    let h = KB_HOOK_HANDLE.swap(0, Ordering::AcqRel);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut c_void));
        }
    }
    // 键盘钩子已卸，无法继续跟踪按键状态；清掉位图，避免下次安装时残留。
    for k in &KEYS_DOWN {
        k.store(0, Ordering::Relaxed);
    }
    KB_CAPTURE_VK.store(0, Ordering::Relaxed);
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
    let px = state.scroll.px_per_notch as f32;
    let units = state.scroll.accum.feed(m.lLastY as f32, px);
    if units != 0 {
        if state.debug {
            eprintln!("[mss-debug] scroll inject {units}");
        }
        queue_wheel(state, units);
    }
}

/// 进入鼠标触发键录入态（菜单「触发键」行点击）。
pub fn arm_capture(state: &mut HostState) {
    // 录入依赖鼠标钩子；功能未开时也要临时装上
    install(state);
    CAPTURING.store(true, Ordering::Release);
    if state.debug {
        eprintln!("[mss-debug] scroll capture armed");
    }
}

/// 进入键盘触发键录入态（菜单「键盘触发键」行点击）。
pub fn arm_kb_capture(state: &mut HostState) {
    install_kb(state);
    KB_CAPTURING.store(true, Ordering::Release);
    KB_CAPTURE_VK.store(0, Ordering::Relaxed);
    if state.debug {
        eprintln!("[mss-debug] kb scroll capture armed");
    }
}

/// 取消鼠标/键盘录入态（静默；宿主侧自行刷新模型）。
pub fn cancel_capture() {
    CAPTURING.store(false, Ordering::Release);
    KB_CAPTURING.store(false, Ordering::Release);
    KB_CAPTURE_VK.store(0, Ordering::Relaxed);
}

/// 录入态被一次按下事件结束（录入成功或取消）：退出录入态并记录该事件的
/// 时间戳，供菜单关闭钩子豁免同一次点击（见 `CAPTURE_END_TIME`）。
fn finish_capture(event_time: u32) {
    CAPTURE_END_TIME.store(event_time, Ordering::Release);
    cancel_capture();
}

/// 该时间戳的事件是否就是「结束录入」的那次按键。菜单关闭钩子用它豁免
/// 这次点击/按键，与两个钩子的调用次序无关。
pub fn is_capture_end_event(event_time: u32) -> bool {
    CAPTURE_END_TIME.load(Ordering::Acquire) == event_time
}

/// 当前是否在录入态（菜单绘制/关菜单钩子判断用）。
pub fn is_capturing() -> bool {
    CAPTURING.load(Ordering::Acquire) || KB_CAPTURING.load(Ordering::Acquire)
}

/// 当前是否在键盘触发键录入态。
pub fn is_kb_capturing() -> bool {
    KB_CAPTURING.load(Ordering::Acquire)
}

/// 菜单开合通知（打开期间滚轮模式放行、关闭时顺带取消录入）。
pub fn set_menu_open(open: bool) {
    MENU_OPEN.store(open, Ordering::Release);
    if !open {
        cancel_capture();
    }
}

// ── 键盘状态位图 ─────────────────────────────────────
// 用 4 个 AtomicU64 记录 0–255 的归一化虚拟键码，支持 O(1) 判断
// 「开关键是否单独按住」以及「其它键是否被按下」。

fn key_index(vk: u32) -> (usize, u64) {
    let c = crate::scroll::KbTrigger { vk }.vk_canonical();
    let idx = (c >> 6) as usize; // 每 64 个键占一个 u64
    let bit = 1u64 << (c & 0x3F);
    (idx.min(3), bit)
}

fn set_key_down(vk: u32) {
    let (idx, bit) = key_index(vk);
    KEYS_DOWN[idx].fetch_or(bit, Ordering::Relaxed);
}

fn set_key_up(vk: u32) {
    let (idx, bit) = key_index(vk);
    KEYS_DOWN[idx].fetch_and(!bit, Ordering::Relaxed);
}

fn is_key_down(vk: u32) -> bool {
    let (idx, bit) = key_index(vk);
    KEYS_DOWN[idx].load(Ordering::Acquire) & bit != 0
}

fn any_key_down_except(trigger_vk: u32) -> bool {
    let c = crate::scroll::KbTrigger { vk: trigger_vk }.vk_canonical();
    for i in 0..4 {
        let v = KEYS_DOWN[i].load(Ordering::Acquire);
        if i == (c >> 6) as usize {
            // 把开关键对应的位清零后再判断
            let bit = 1u64 << (c & 0x3F);
            if (v & !bit) != 0 {
                return true;
            }
        } else if v != 0 {
            return true;
        }
    }
    false
}

/// 返回当前是否有任何非注入键被按住（用于判定「开关键单独按住」）。
fn kb_only_trigger_held(trigger: KbTrigger) -> (bool, bool) {
    let trigger_down = is_key_down(trigger.vk);
    let others_down = any_key_down_except(trigger.vk);
    (trigger_down, !others_down)
}

/// 根据当前键盘位图更新滚轮模式键盘来源状态。
fn update_kb_active(state: &mut HostState) {
    if let Some(t) = state.scroll.kb_trigger {
        let (trigger_down, alone) = kb_only_trigger_held(t);
        state.scroll.kb_held = trigger_down && alone;
    } else {
        state.scroll.kb_held = false;
    }
    state.scroll.sync_active();
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
                } else if !MENU_OPEN.load(Ordering::Acquire) && state.scroll.enabled {
                    let t = state.scroll.trigger;
                    if matches_down(msg, info.mouseData, t) {
                        // 进入滚轮模式：吞掉鼠标触发键按下。位移源是 Raw Input，
                        // 这里不再记基准点（见模块头注释 / 踩坑 四-17）
                        state.scroll.mouse_held = true;
                        state.scroll.sync_active();
                        if state.debug {
                            eprintln!(
                                "[mss-debug] scroll mode ON (raw input, {} px/notch)",
                                state.scroll.px_per_notch
                            );
                        }
                        return LRESULT(1);
                    }
                    if state.scroll.active {
                        if matches_up(msg, info.mouseData, t) {
                            state.scroll.mouse_held = false;
                            state.scroll.sync_active();
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
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn keyboard_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let ptr = HOST_PTR.load(Ordering::Acquire);
        if ptr != 0 {
            let state = &mut *(ptr as *mut HostState);
            let info = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            let msg = wp.0 as u32;
            // 忽略注入事件（包括我们自己发的滚轮不会走这里，但其它程序注入的键盘要放行）
            if (info.flags & LLKHF_INJECTED).0 == 0 {
                let vk = info.vkCode;
                let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

                if down {
                    set_key_down(vk);
                } else if up {
                    set_key_up(vk);
                }

                // 诊断：录制态打印所有键盘事件
                if state.debug && KB_CAPTURING.load(Ordering::Acquire) {
                    eprintln!(
                        "[mss-debug] kb capture evt msg={:#06x} vk={:#04x} flags={:#x}",
                        msg, vk, info.flags.0
                    );
                }

                // ── 键盘触发键录制 ──
                if KB_CAPTURING.load(Ordering::Acquire) && down {
                    if vk == 0x1B {
                        // Esc 取消
                        KB_CAPTURING.store(false, Ordering::Release);
                        CAPTURE_END_TIME.store(info.time, Ordering::Release);
                        notify_kb_host(state, false, 0);
                        if state.debug {
                            eprintln!("[mss-debug] kb scroll capture cancelled (Esc)");
                        }
                        return LRESULT(1);
                    }
                    let canon = crate::scroll::KbTrigger { vk }.vk_canonical();
                    KB_CAPTURE_VK.store(canon, Ordering::Relaxed);
                    KB_CAPTURING.store(false, Ordering::Release);
                    CAPTURE_END_TIME.store(info.time, Ordering::Release);
                    // 录制期间吞掉该次按键，避免菜单/应用误处理
                    notify_kb_host(state, true, canon);
                    if state.debug {
                        eprintln!("[mss-debug] kb scroll capture recorded vk={:#04x}", canon);
                    }
                    return LRESULT(1);
                }

                // 若正在录制的键抬起，吞掉这次 up，防止菜单/应用误收到；
                // 同时清掉 KB_CAPTURE_VK，避免后续同键释放也被吞。
                if up {
                    let up_canon = crate::scroll::KbTrigger { vk }.vk_canonical();
                    if up_canon == KB_CAPTURE_VK.load(Ordering::Acquire) {
                        KB_CAPTURE_VK.store(0, Ordering::Relaxed);
                        // 仍要按当前键盘状态重算 kb_held（例如用户录制的就是触发键）
                        if !MENU_OPEN.load(Ordering::Acquire)
                            && state.scroll.enabled
                            && state.scroll.kb_trigger.is_some()
                        {
                            update_kb_active(state);
                        }
                        return LRESULT(1);
                    }
                }

                // ── 滚轮模式键盘触发 ──
                if !MENU_OPEN.load(Ordering::Acquire)
                    && state.scroll.enabled
                    && state.scroll.kb_trigger.is_some()
                {
                    update_kb_active(state);

                    // 调试输出
                    if state.debug && (down || up) {
                        eprintln!(
                            "[mss-debug] kb evt vk={:#04x} active={} kb_held={}",
                            vk, state.scroll.active, state.scroll.kb_held
                        );
                    }

                    // 若开关键被单独按住且进入激活：吞掉鼠标移动由另一个钩子负责，
                    // 键盘事件不灭活，让系统/应用仍能收到。
                    if state.scroll.active && !state.scroll.kb_held {
                        // 组合键触发 → 取消滚轮模式
                        if state.debug {
                            eprintln!("[mss-debug] scroll mode OFF (combo triggered)");
                        }
                        state.scroll.sync_active();
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

/// 鼠标触发键录入结束 → 通知宿主线程收尾（落盘 / 刷新菜单模型）。
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

/// 键盘触发键录入结束 → 通知宿主线程。
/// `recorded` = true 时 `vk` 为归一化虚拟键码；false = 取消。
fn notify_kb_host(state: &HostState, recorded: bool, vk: u32) {
    unsafe {
        let _ = PostMessageW(
            Some(state.hwnd),
            WM_APP_KB_SCROLL_CHANGED,
            WPARAM(recorded as usize),
            LPARAM(vk as isize),
        );
    }
}
