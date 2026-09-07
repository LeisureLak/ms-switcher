//! Shell_NotifyIconW 托盘图标。
//!
//! 事件通过应用私有消息 `WM_APP_TRAY` 投递到宿主窗口，不再需要独立事件
//! 线程（对应迁移计划第 7 节）。版本使用 NOTIFYICON_VERSION_4：
//! `lparam` 低 16 位为通知事件（WM_LBUTTONUP 等）。
//!
//! 托盘图标现在从本 exe 嵌入的 `assets/icon.ico`（资源 ID 1）加载，与 exe 图标一致。

#![allow(unsafe_op_in_unsafe_fn)]
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetSystemMetrics, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXSMICON,
    WM_APP, WM_LBUTTONUP, WM_RBUTTONUP,
};

/// 托盘回调消息（应用私有）。
pub const WM_APP_TRAY: u32 = WM_APP + 1;
const UID: u32 = 1;

/// 通知区域图标边长（物理像素）。进程已 PMv2，SM_CXSMICON 随 DPI 缩放。
fn tray_icon_size() -> u32 {
    let v = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if v > 0 { v as u32 } else { 16 }
}

/// 从本 exe 嵌入的资源（assets/icon.ico，资源 ID 1）加载 HICON。
fn load_tray_icon() -> Option<HICON> {
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }.ok()?.into();
    let size = tray_icon_size() as i32;
    let handle = unsafe {
        LoadImageW(
            Some(hinstance),
            PCWSTR::from_raw(1 as *const u16),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    }
    .ok()?;
    Some(HICON(handle.0 as *mut _))
}

fn wide(buf: &mut [u16], s: &str) {
    let len = s.encode_utf16().take(buf.len() - 1).collect::<Vec<_>>();
    buf[..len.len()].copy_from_slice(&len);
    buf[len.len()] = 0;
}

/// 托盘图标。Drop 时自动移除图标并销毁 HICON。
pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    added: bool,
    tip: String,
}

impl Tray {
    /// 添加托盘图标；失败返回 None。
    pub fn new(hwnd: HWND, tip: &str) -> Option<Tray> {
        let icon = load_tray_icon()?;
        let mut t = Tray {
            hwnd,
            icon,
            added: false,
            tip: String::new(),
        };
        if t.add(tip) { Some(t) } else { None }
    }

    fn nid(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: UID,
            // NIF_ICON 必不可少：缺了它 Shell 不读 hIcon，托盘显示空白占位
            uFlags: { NIF_MESSAGE | NIF_ICON | NIF_TIP },
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        wide(&mut nid.szTip, &self.tip);
        nid
    }

    /// NIM_ADD + NIM_SETVERSION。Explorer 重启恢复时也走这里。
    pub fn add(&mut self, tip: &str) -> bool {
        self.tip = tip.to_string();
        let mut nid = self.nid();
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &mut nid) }.as_bool() {
            return false;
        }
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &mut nid) }.as_bool() {
            return false;
        }
        self.added = true;
        true
    }

    /// 文本实际变化才 NIM_MODIFY（tooltip 去重）。
    pub fn set_tip(&mut self, tip: &str) {
        if tip == self.tip || !self.added {
            return;
        }
        self.tip = tip.to_string();
        let mut nid = self.nid();
        nid.uFlags &= !NIF_MESSAGE; // MODIFY 不需要重设回调消息
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &mut nid);
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            let mut nid = self.nid();
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
            }
            self.added = false;
        }
    }

    /// 托盘事件是否为「左/右键抬起」。
    pub fn is_click_up(lparam: isize) -> bool {
        let ev = (lparam & 0xFFFF) as u32;
        ev == WM_LBUTTONUP || ev == WM_RBUTTONUP
    }

    /// 最近一次设置的 tooltip 文本（TaskbarCreated 恢复时重用）。
    pub fn last_tip(&self) -> &str {
        &self.tip
    }
}

/// 托盘图标 ID（NIM_SETFOCUS 等重建 nid 时需要）。
pub fn uid() -> u32 {
    UID
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        unsafe {
            let _ = DestroyIcon(self.icon);
        }
    }
}
