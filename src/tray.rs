use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{HICON, WM_APP, WM_LBUTTONUP, WM_RBUTTONUP};

/// 托盘图标回调消息（WM_APP + 1）。
pub const WM_TRAYICON: u32 = WM_APP + 1;

pub struct Tray {
    nid: NOTIFYICONDATAW,
}

// Tray 只被创建它的主线程（窗口过程）访问；static Mutex 仅用于让窗口过程拿到可变引用。
// NOTIFYICONDATAW 内含 HWND（裸指针），标记 Send 在本程序单线程使用场景下是安全的。
unsafe impl Send for Tray {}

impl Tray {
    /// 添加托盘图标；失败返回 None。
    pub fn add(hwnd: HWND, icon: HICON) -> Option<Tray> {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = icon;
        set_tip(&mut nid, "MouseSpeedSwitcher");
        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
        if !ok.as_bool() {
            return None;
        }
        Some(Tray { nid })
    }

    pub fn set_tip(&mut self, tip: &str) {
        set_tip(&mut self.nid, tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.nid);
        }
    }

    pub fn remove(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid);
        }
    }
}

fn set_tip(nid: &mut NOTIFYICONDATAW, tip: &str) {
    let mut chars = tip.encode_utf16();
    for slot in nid.szTip.iter_mut() {
        *slot = chars.next().unwrap_or(0);
    }
}

/// 托盘事件是否应弹出菜单（左键或右键抬起）。
pub fn is_click(msg: u32, lparam: isize) -> bool {
    let e = lparam as u32;
    msg == WM_TRAYICON && (e == WM_LBUTTONUP as u32 || e == WM_RBUTTONUP as u32)
}
