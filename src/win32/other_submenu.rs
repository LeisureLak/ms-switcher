//! 「其他设备」列表子菜单窗口：展示所有无规则设备。
//!
//! - 行为与主菜单设备区相似：每行悬停打开右侧单设备配置子菜单。
//! - `WS_POPUP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`：不抢焦点。
//! - owner = 主菜单：主菜单销毁时级联销毁。
//! - 跨窗口悬停保持靠 `model.other_pointer_inside` 与 `model.sub_pointer_inside` 配合。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, FrameRect, InvalidateRect, PAINTSTRUCT, SelectObject,
    SetBkMode, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging as win;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW,
    GetWindowRect, PostMessageW, SW_SHOWNA, SetWindowLongPtrW, ShowWindow, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

use crate::menu_model as menu_model;
use crate::menu_model::{
    INFO_ROW_H, MENU_W, OTHER_SUB_W, PAD, ROW_H, TOP_PAD, other_sub_height, other_sub_row_at,
    other_sub_row_top,
};

use super::host::{HostState, WM_APP_CLOSE_OTHER_SUBMENU};
use super::menu::{apply_dwm, current_pal, dip_from_lp, dpi_scale, draw_row_bg, draw_text};
use super::submenu;

const OTHER_SUB_CLASS: windows::core::PCWSTR = w!("MSS_OtherSubMenu");

/// 光标是否位于窗口矩形内。
pub fn cursor_inside_window(hwnd: HWND) -> bool {
    unsafe {
        let mut pt = POINT::default();
        let _ = win::GetCursorPos(&mut pt);
        let mut wr = RECT::default();
        if win::GetWindowRect(hwnd, &mut wr).is_ok() {
            pt.x >= wr.left && pt.x < wr.right && pt.y >= wr.top && pt.y < wr.bottom
        } else {
            false
        }
    }
}

/// 光标是否位于当前其他设备列表子菜单内。
pub fn is_cursor_inside(state: &HostState) -> bool {
    state
        .other_sub
        .map_or(false, |h| cursor_inside_window(h))
}

/// 注册其他设备列表子菜单窗口类（进程内一次）。
pub fn register_class(hinstance: HINSTANCE) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let wc = win::WNDCLASSEXW {
            cbSize: std::mem::size_of::<win::WNDCLASSEXW>() as u32,
            style: win::CS_HREDRAW | win::CS_VREDRAW,
            lpfnWndProc: Some(other_sub_wndproc),
            hInstance: hinstance,
            hCursor: win::LoadCursorW(None, windows::Win32::UI::WindowsAndMessaging::IDC_ARROW)
                .unwrap_or_default(),
            lpszClassName: OTHER_SUB_CLASS,
            ..Default::default()
        };
        assert_ne!(win::RegisterClassExW(&wc), 0, "register other submenu class");
    });
}

/// 按模型状态开/关/切换其他设备列表子菜单窗口（主菜单 WM_MOUSEMOVE/LEAVE 驱动）。
pub fn sync(state: &mut HostState, menu_hwnd: HWND) {
    let want = state.model.other_entry_hovered;
    match (want, state.other_sub) {
        (true, None) => {
            if let Err(e) = open(state, menu_hwnd) {
                if state.debug {
                    eprintln!("[mss-debug] open other submenu failed: {e}");
                }
            }
        }
        (false, Some(_)) if !state.model.other_pointer_inside => {
            close(state);
        }
        _ => {
            if state.other_sub.is_some() {
                unsafe {
                    let _ = InvalidateRect(state.other_sub, None, false);
                }
            }
        }
    }
}

/// 打开其他设备列表子菜单：紧贴主菜单右缘，顶对齐「其他设备」入口。
fn open(state: &mut HostState, menu_hwnd: HWND) -> Result<HWND, windows::core::Error> {
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    register_class(hinstance);

    let n_other = state.model.other_devs.len();
    let mut mr = RECT::default();
    unsafe {
        let _ = GetWindowRect(menu_hwnd, &mut mr);
    }
    let s = dpi_scale(menu_hwnd);
    let sub_w = (OTHER_SUB_W * s).round() as i32;
    let sub_h = (other_sub_height(n_other) * s).round() as i32;

    // 位置：默认紧贴主菜单右缘；右侧出工作区则改为左缘展开。
    let (work, _menu_mon) = unsafe {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        };
        let mon = MonitorFromWindow(menu_hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO::default();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let _ = GetMonitorInfoW(mon, &mut mi);
        (mi, mon)
    };
    let x_right = mr.left + (MENU_W * s).round() as i32;
    let x = if x_right + sub_w > work.rcWork.right && mr.left - sub_w >= work.rcWork.left {
        mr.left - sub_w
    } else {
        x_right
    };
    let entry_top = menu_model::other_row_top(state.model.n_ruled());
    let y = (mr.top + (entry_top * s).round() as i32).clamp(
        work.rcWork.top,
        (work.rcWork.bottom - sub_h).max(work.rcWork.top),
    );

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            OTHER_SUB_CLASS,
            w!("MSS Other Devices"),
            WS_POPUP,
            x,
            y,
            sub_w,
            sub_h,
            Some(menu_hwnd),
            None,
            Some(hinstance.into()),
            Some(state.hwnd.0.cast()),
        )?
    };

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    apply_dwm(hwnd, &current_pal(state));
    state.other_sub = Some(hwnd);
    state.other_sub_hover = None;
    state.other_sub_pressed = None;
    Ok(hwnd)
}

/// 关闭其他设备列表子菜单（主菜单保持打开）。
pub fn close(state: &mut HostState) {
    if let Some(h) = state.other_sub.take() {
        state.other_sub_hover = None;
        state.other_sub_pressed = None;
        state.model.other_dev_hover = None;
        state.model.other_pointer_inside = false;
        // 单设备配置子菜单若由本窗口展开，随本窗口级联销毁；
        // 仍调用 submenu::close 清理 HostState 中的句柄。
        super::submenu::close(state);
        unsafe {
            let _ = win::DestroyWindow(h);
        }
    }
}

/// 其他设备列表子菜单窗口过程。
pub unsafe extern "system" fn other_sub_wndproc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lp.0 as *const CREATESTRUCTW;
        SetWindowLongPtrW(hwnd, win::GWLP_USERDATA, (*cs).lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let host = HWND(GetWindowLongPtrW(hwnd, win::GWLP_USERDATA) as *mut c_void);
    if host.is_invalid() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ptr = GetWindowLongPtrW(host, win::GWLP_USERDATA) as *mut HostState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }

    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(ptr, hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let state = &mut *ptr;
            state.model.other_pointer_inside = true;
            let (_, y) = dip_from_lp(hwnd, lp);
            let n_other = state.model.other_devs.len();
            let local = other_sub_row_at(y, n_other);
            if state.other_sub_hover != local {
                state.other_sub_hover = local;
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            if let Some(i) = local {
                let top = other_sub_row_top(i);
                state.model.update_other_hover(Some(i), top);
                // 让单设备配置子菜单以本窗口为 owner 展开
                submenu::sync(state, hwnd);
            } else {
                state.model.update_other_hover(None, 0.0);
                submenu::sync(state, hwnd);
            }
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let state = &mut *ptr;
            state.other_sub_pressed = None;
            if cursor_inside_window(hwnd) {
                return LRESULT(0);
            }
            state.model.other_pointer_inside = false;
            state.other_sub_hover = None;
            let _ = InvalidateRect(Some(hwnd), None, false);
            // 若光标在单设备配置子菜单内，保持本列表打开
            let inside_sub = state
                .sub
                .map_or(false, |h| submenu::cursor_inside_window(h));
            if !inside_sub {
                // 若主菜单没悬停在「其他设备」入口，请求关闭
                if !state.model.other_entry_hovered {
                    state.model.other_dev_hover = None;
                    state.model.sub_hover = None;
                    let _ = PostMessageW(Some(host), WM_APP_CLOSE_OTHER_SUBMENU, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let state = &mut *ptr;
            let (_, y) = dip_from_lp(hwnd, lp);
            let n_other = state.model.other_devs.len();
            state.other_sub_pressed = other_sub_row_at(y, n_other);
            if state.other_sub_pressed.is_some() {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let state = &mut *ptr;
            let (_, y) = dip_from_lp(hwnd, lp);
            let n_other = state.model.other_devs.len();
            let pressed = state.other_sub_pressed.take();
            let now = other_sub_row_at(y, n_other);
            let _ = InvalidateRect(Some(hwnd), None, false);
            if pressed == now && pressed.is_some() {
                // 点击其他设备行：当前行为与悬停一致（展开配置子菜单），
                // 后续若需要添加「直接用当前速度保存规则」可在此扩展。
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn paint(ptr: *mut HostState, hwnd: HWND) {
    unsafe {
        let state = &mut *ptr;
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let w = rc.right.max(1);
        let h = rc.bottom.max(1);

        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old_bmp = SelectObject(mem, bmp.into());
        let s = dpi_scale(hwnd);
        let pal = current_pal(state);

        // 背景 + 边框
        let full = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        let bg_brush = CreateSolidBrush(pal.bg);
        FillRect(mem, &full, bg_brush);
        let _ = DeleteObject(bg_brush.into());
        let border_brush = CreateSolidBrush(pal.border);
        FrameRect(mem, &full, border_brush);
        let _ = DeleteObject(border_brush.into());

        let old_font = SelectObject(mem, super::menu::ui_font(state).into());
        SetBkMode(mem, TRANSPARENT);

        let n_other = state.model.other_devs.len();
        if n_other == 0 {
            let top = TOP_PAD + INFO_ROW_H / 2.0;
            draw_text(mem, "无其他设备", PAD, top, s, pal.gray);
        } else {
            for local in 0..n_other {
                let top = other_sub_row_top(local);
                let global = state.model.other_devs[local];
                let d = &state.model.devs[global];
                let hovered = state.other_sub_hover == Some(local);
                let pressed = state.other_sub_pressed == Some(local);
                let color = if hovered || pressed {
                    pal.hl_text
                } else {
                    pal.text
                };
                draw_row_bg(
                    mem,
                    top,
                    ROW_H,
                    s,
                    w,
                    hovered,
                    pressed,
                    false,
                    pal.highlight,
                    pal.row_pressed,
                );
                draw_text(mem, &d.row_text(), PAD, top + ROW_H / 2.0, s, color);
            }
        }

        SelectObject(mem, old_font);
        let _ = windows::Win32::Graphics::Gdi::BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old_bmp);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}
