//! 输入探测工具：打印所有底层鼠标/键盘事件，用于诊断按键识别问题。
//!
//! 运行后随便按鼠标各键和按键，每条输入打印一行原始消息：
//! `mouse msg=0x020b data=0x00010000 injected=no` / `key vk=0xa4 injected=no`
//! Ctrl+C 退出。

#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, SetWindowsHookExW,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_MOUSEMOVE,
};

fn mouse_msg_name(msg: u32) -> &'static str {
    match msg {
        0x0200 => "MOVE",
        0x0201 => "L-DOWN",
        0x0202 => "L-UP",
        0x0204 => "R-DOWN",
        0x0205 => "R-UP",
        0x0207 => "M-DOWN",
        0x0208 => "M-UP",
        0x020A => "WHEEL",
        0x020B => "X1/X2-DOWN",
        0x020C => "X1/X2-UP",
        0x020E => "HWHEEL",
        _ => "OTHER",
    }
}

fn key_name(vk: u32) -> String {
    match vk {
        0x01..=0x06 => "mouse-ish".into(),
        0xA4 => "Alt".into(),
        0xA5 => "AltGr".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2F => "Sleep".into(),
        0xA6 => "BrowserBack".into(),
        0xA7 => "BrowserForward".into(),
        0xB0 => "MediaNext".into(),
        0xB1 => "MediaPrev".into(),
        _ => format!("vk=0x{vk:02x}"),
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let msg = wp.0 as u32;
        if msg != WM_MOUSEMOVE || info.flags != 0 {
            println!(
                "mouse msg={:#06x} ({}) xbtn_hi={} wheel_hi={} flags={:#x} injected={}",
                msg,
                mouse_msg_name(msg),
                info.mouseData >> 16,
                (info.mouseData as i32) >> 16,
                info.flags,
                info.flags & 1 != 0
            );
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn key_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        // LLKHF_INJECTED = 0x10
        let f = info.flags.0;
        println!(
            "key {} down={} injected={} flags={:#x}",
            key_name(info.vkCode),
            wp.0 as u32 == 0x0100, // WM_KEYDOWN / WM_SYSKEYDOWN
            f & 0x10 != 0,
            f
        );
    }
    CallNextHookEx(None, code, wp, lp)
}

fn main() {
    println!("输入探测中：按鼠标各键 / 侧键 / 中键，输出原始消息。Ctrl+C 退出。\n");
    unsafe {
        let m = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0).expect("mouse hook");
        let k = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), None, 0).expect("key hook");
        let mut msg = Default::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // 仅泵消息维持钩子；不做分发（无窗口）
            if msg.message == 0x0012 {
                break;
            }
        }
        let _ = windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(m);
        let _ = windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(k);
    }
}
