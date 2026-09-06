//! Shell_NotifyIconW 托盘图标。
//!
//! 事件通过应用私有消息 `WM_APP_TRAY` 投递到宿主窗口，不再需要独立事件
//! 线程（对应迁移计划第 7 节）。版本使用 NOTIFYICON_VERSION_4：
//! `lparam` 低 16 位为通知事件（WM_LBUTTONUP 等）。

#![allow(unsafe_op_in_unsafe_fn)]
use std::ffi::c_void;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON, WM_APP,
    WM_LBUTTONUP, WM_RBUTTONUP,
};

/// 托盘回调消息（应用私有）。
pub const WM_APP_TRAY: u32 = WM_APP + 1;
const UID: u32 = 1;

/// 通知区域图标边长（物理像素）。进程已 PMv2，SM_CXSMICON 随 DPI 缩放。
fn tray_icon_size() -> u32 {
    let v = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if v > 0 { v as u32 } else { 16 }
}

/// 生成程序图标的 S×S RGBA 像素（深蓝圆 + 白色指针）。
fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as usize;
    let mut rgba = vec![0u8; s * s * 4];
    let c = (size as f32 - 1.0) / 2.0;
    let k = size as f32 / 32.0; // 原始设计稿按 32px 标注，按尺寸等比缩放
    for y in 0..s {
        for x in 0..s {
            let dx = (x as f32 - c) / k;
            let dy = (y as f32 - c) / k;
            let d = ((dx * dx + dy * dy).sqrt()) * k;
            let i = (y * s + x) * 4;
            if d <= c {
                // 深蓝圆底
                let (r, g, b) = (0x00, 0x78, 0xD7);
                // 中间画一个白色小三角（模拟指针）
                let inside_triangle =
                    dy > -2.0 && dy < 8.0 && dx > -6.0 + dy * 0.45 && dx < -1.0 + dy * 0.45;
                if inside_triangle {
                    rgba[i] = 0xFF;
                    rgba[i + 1] = 0xFF;
                    rgba[i + 2] = 0xFF;
                } else {
                    rgba[i] = r;
                    rgba[i + 1] = g;
                    rgba[i + 2] = b;
                }
                rgba[i + 3] = 0xFF;
            }
        }
    }
    rgba
}

/// 把 RGBA 像素转成带 alpha 通道的 HICON（32bpp 自上而下 DIB + 单色掩码）。
fn hicon_from_rgba(size: u32, rgba: &[u8]) -> Option<HICON> {
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let hbmp_color = unsafe {
        CreateDIBSection(
            None,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
    }
    .ok()?;
    if bits.is_null() {
        return None;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), bits as *mut u8, (size * size * 4) as usize);
    }
    // 掩码位图必须显式清零：CreateBitmap 不初始化位数据，残留垃圾会把
    // 像素整体掩掉（32bpp 图标虽有 alpha 通道，掩码仍需保证全透明明示）
    let mask_bits = vec![0u8; ((size + 15) / 16 * 2 * size) as usize];
    let hbmp_mask = unsafe { CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bits.as_ptr() as *const c_void)) };
    if hbmp_mask.is_invalid() {
        unsafe { let _ = DeleteObject(hbmp_color.into()); }
        return None;
    }
    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: hbmp_mask,
        hbmColor: hbmp_color,
    };
    let icon = unsafe { CreateIconIndirect(&info) };
    // CreateIconIndirect 会复制位图，临时位图立即释放
    unsafe {
        let _ = DeleteObject(hbmp_color.into());
        let _ = DeleteObject(hbmp_mask.into());
    }
    let _ = &mut bmi;
    icon.ok()
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
        let size = tray_icon_size();
        let rgba = icon_rgba(size);
        let icon = hicon_from_rgba(size, &rgba)?;
        let mut t = Tray { hwnd, icon, added: false, tip: String::new() };
        if t.add(tip) {
            Some(t)
        } else {
            None
        }
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
