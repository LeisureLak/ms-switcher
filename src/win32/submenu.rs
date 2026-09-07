//! 设备子菜单窗口：VID/PID 信息 + 速度滑块（本地预览）+「设为规则」按钮 +
//! 保存/删除/重新应用规则。
//!
//! - `WS_POPUP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`：纯交互小窗不抢焦点，
//!   主菜单保持前台，失焦关闭逻辑不受影响（踩坑 三-4 的结构性规避）。
//! - owner = 主菜单：主菜单销毁时级联销毁，无孤儿窗口。
//! - 跨窗口悬停保持：主菜单收不到指针时靠 `model.sub_pointer_inside`
//!   保持子菜单打开（与 egui 版语义一致，逻辑在纯模型 update_hover）。
//! - 子菜单滑块是本地预览值（`model.sub_slider`）：拖动只改预览，
//!   点「设为规则」按钮才把该值写入设备规则并重新应用——不直接碰系统速度。
//! - 关闭一律走「先摘除再销毁」或异步 `WM_APP_CLOSE_SUBMENU`（处理时
//!   重新检查悬停状态），杜绝同步销毁级联（踩坑 三-1/三-3）。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, FrameRect, InvalidateRect, PAINTSTRUCT, PS_SOLID, SRCCOPY,
    SelectObject, SetBkMode, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    TBM_SETPOS, TBM_SETRANGE, TBS_HORZ, TBS_NOTICKS, TRACKBAR_CLASS, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging as win;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW,
    GetWindowRect, HMENU, PostMessageW, SW_SHOWNA, SendMessageW, SetWindowLongPtrW, ShowWindow,
    WM_ERASEBKGND, WM_HSCROLL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE,
    WM_NOTIFY, WM_PAINT,
    WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::w;

use crate::menu_model::{
    DevRow, Hover, MenuAction, PAD, SUB_INFO_TOP, SUB_ROW_H, SUB_W, sub_action_top, sub_btn_at,
    sub_btn_rect, sub_height, sub_kb_trigger_at, sub_kb_trigger_top, sub_ptr_label_top,
    sub_row_at, sub_scroll_mode_at, sub_scroll_mode_top, sub_scroll_sens_label_top,
    sub_scroll_sens_rect, sub_scroll_trigger_at, sub_scroll_trigger_top, sub_slider_rect,
    sub_wheel_label_top, sub_wheel_rect,
};
use crate::scroll::{SCROLL_PX_DEFAULT, SCROLL_PX_MAX, SCROLL_PX_MIN};

use super::host::{HostState, WM_APP_CLOSE_SUBMENU};
use super::menu::{
    SUB_CLASS, apply_dwm, current_pal, dip_from_lp, dpi_scale, draw_check, draw_text,
    draw_text_center, register_class, trackbar_notify, trackbar_proc, trackbar_theme,
};

/// TBM_GETPOS 未包含在 windows crate 绑定中，值为 WM_USER（与 menu.rs 一致）。
const TBM_GETPOS: u32 = win::WM_USER;

/// 按模型子菜单状态开/关/切换子菜单窗口（主菜单/other 列表 WM_MOUSEMOVE/LEAVE 驱动）。
/// `owner_hwnd` 为单设备配置子菜单贴靠的父窗口（主菜单或「其他设备」列表）。
pub fn sync(state: &mut HostState, owner_hwnd: HWND) {
    let want = state.model.sub_hover;
    match (want, state.sub) {
        (_, Some(_)) if state.sub_dev == want.map(|(i, _)| i) => {}
        (Some((idx, top)), cur) => {
            if cur.is_some() {
                close(state);
            }
            if let Err(e) = open(state, owner_hwnd, idx, top) {
                if state.debug {
                    eprintln!("[mss-debug] open submenu failed: {e}");
                }
            }
        }
        (None, Some(_)) => close(state),
        _ => {}
    }
}

/// 打开设备子菜单：紧贴 owner 右缘、顶对齐悬停行；不抢焦点。
fn open(
    state: &mut HostState,
    owner_hwnd: HWND,
    idx: usize,
    row_top: f32,
) -> Result<HWND, windows::core::Error> {
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    register_class(hinstance);

    let mut mr = RECT::default();
    unsafe {
        let _ = GetWindowRect(owner_hwnd, &mut mr);
    }
    let s = dpi_scale(owner_hwnd);
    let sub_w = (SUB_W * s).round() as i32;
    let sub_h = (sub_height() * s).round() as i32;

    // 位置：默认紧贴 owner 右缘；右侧出工作区则改为左缘展开；垂直方向
    // 顶对齐悬停行并夹取到工作区内（多显示器按 owner 所在显示器计算）。
    let (work, _menu_mon) = unsafe {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        };
        let mon = MonitorFromWindow(owner_hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO::default();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let _ = GetMonitorInfoW(mon, &mut mi);
        (mi, mon)
    };
    let x_right = mr.right; // owner 右缘（物理像素）
    let x = if x_right + sub_w > work.rcWork.right && mr.left - sub_w >= work.rcWork.left {
        mr.left - sub_w // 右侧放不下：贴 owner 左缘
    } else {
        x_right
    };
    let y = (mr.top + (row_top * s).round() as i32).clamp(
        work.rcWork.top,
        (work.rcWork.bottom - sub_h).max(work.rcWork.top),
    );

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            SUB_CLASS,
            w!("MSS SubMenu"),
            WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            x,
            y,
            sub_w,
            sub_h,
            Some(owner_hwnd), // owner：父窗口销毁时级联销毁
            None,
            Some(hinstance.into()),
            Some(state.hwnd.0.cast()),
        )?
    };

    // 滑块初值：有规则用规则值，无规则用当前系统值（纯本地预览）
    let d = state.model.devs.get(idx);
    let (init_sp, init_wh) = match d {
        Some(d) => (
            d.rule_speed.unwrap_or_else(crate::speed::get),
            d.rule_wheel.unwrap_or_else(crate::speed::get_wheel),
        ),
        None => (crate::speed::get(), crate::speed::get_wheel()),
    };
    state.model.sub_slider = Some(init_sp);
    state.model.sub_wheel = Some(init_wh);
    // 滚轮模式初值：有规则取规则，否则无（未启用）
    state.model.sub_scroll = d.and_then(|d| d.rule_scroll.clone());

    // 指针滑块（1–20）
    let (sl, st, sr, sb) = sub_slider_rect();
    let tb = unsafe {
        CreateWindowExW(
            win::WINDOW_EX_STYLE(0),
            TRACKBAR_CLASS,
            w!(""),
            WS_CHILD | WS_VISIBLE | win::WINDOW_STYLE(TBS_HORZ | TBS_NOTICKS),
            ((sl * s).round()) as i32,
            ((st * s).round()) as i32,
            (((sr - sl) * s).round()) as i32,
            (((sb - st) * s).round()) as i32,
            Some(hwnd),
            Some(HMENU(1 as *mut c_void)),
            Some(hinstance.into()),
            None,
        )?
    };
    // 滚轮滑块（1–100）
    let (wl, wt, wr, wb) = sub_wheel_rect();
    let tb_wh = unsafe {
        CreateWindowExW(
            win::WINDOW_EX_STYLE(0),
            TRACKBAR_CLASS,
            w!(""),
            WS_CHILD | WS_VISIBLE | win::WINDOW_STYLE(TBS_HORZ | TBS_NOTICKS),
            ((wl * s).round()) as i32,
            ((wt * s).round()) as i32,
            (((wr - wl) * s).round()) as i32,
            (((wb - wt) * s).round()) as i32,
            Some(hwnd),
            Some(HMENU(2 as *mut c_void)),
            Some(hinstance.into()),
            None,
        )?
    };
    // 滚轮模式灵敏度滑块（2–200 像素/行）
    let (xl, xt, xr, xb) = sub_scroll_sens_rect();
    let tb_scroll = unsafe {
        CreateWindowExW(
            win::WINDOW_EX_STYLE(0),
            TRACKBAR_CLASS,
            w!(""),
            WS_CHILD | WS_VISIBLE | win::WINDOW_STYLE(TBS_HORZ | TBS_NOTICKS),
            ((xl * s).round()) as i32,
            ((xt * s).round()) as i32,
            (((xr - xl) * s).round()) as i32,
            (((xb - xt) * s).round()) as i32,
            Some(hwnd),
            Some(HMENU(5 as *mut c_void)),
            Some(hinstance.into()),
            None,
        )?
    };
    let init_px = state
        .model
        .sub_scroll
        .as_ref()
        .map_or(SCROLL_PX_DEFAULT, |s| s.px_per_line);
    unsafe {
        SendMessageW(
            tb,
            TBM_SETRANGE,
            Some(WPARAM(0)),
            Some(LPARAM(makelong(1, 20) as isize)),
        );
        SendMessageW(
            tb,
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(init_sp as isize)),
        );
        SendMessageW(
            tb_wh,
            TBM_SETRANGE,
            Some(WPARAM(0)),
            Some(LPARAM(makelong(1, 100) as isize)),
        );
        SendMessageW(
            tb_wh,
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(init_wh as isize)),
        );
        SendMessageW(
            tb_scroll,
            TBM_SETRANGE,
            Some(WPARAM(0)),
            Some(LPARAM(
                makelong(SCROLL_PX_MIN as i32, SCROLL_PX_MAX as i32) as isize
            )),
        );
        SendMessageW(
            tb_scroll,
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(init_px as isize)),
        );
        // 子类 id 3/4/5：Esc 转发 + 子控件 leave 通知（光标从滑块直接移出窗口的路径）
        let _ = windows::Win32::UI::Shell::SetWindowSubclass(
            tb,
            Some(trackbar_proc),
            3,
            state.hwnd.0 as usize,
        );
        let _ = windows::Win32::UI::Shell::SetWindowSubclass(
            tb_wh,
            Some(trackbar_proc),
            4,
            state.hwnd.0 as usize,
        );
        let _ = windows::Win32::UI::Shell::SetWindowSubclass(
            tb_scroll,
            Some(trackbar_proc),
            5,
            state.hwnd.0 as usize,
        );

        trackbar_theme(tb, state.theme_dark, state.hc);
        trackbar_theme(tb_wh, state.theme_dark, state.hc);
        trackbar_theme(tb_scroll, state.theme_dark, state.hc);
    }
    state.sub_trackbar = Some(tb);
    state.sub_wheel_trackbar = Some(tb_wh);
    state.sub_scroll_trackbar = Some(tb_scroll);

    // 未启用滚轮模式时灵敏度滑块置灰
    let scroll_enabled = state.model.sub_scroll.as_ref().map_or(false, |s| s.enabled);
    unsafe {
        let _ = EnableWindow(tb_scroll, scroll_enabled);
    }

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNA); // 不激活，主菜单保持前台
    }
    apply_dwm(hwnd, &current_pal(state));
    state.sub = Some(hwnd);
    state.sub_dev = Some(idx);
    state.sub_hover_row = None;
    Ok(hwnd)
}

fn makelong(lo: i32, hi: i32) -> u32 {
    (lo as u16 as u32) | ((hi as u16 as u32) << 16)
}

/// 光标是否位于窗口矩形内。子菜单含滑块子控件：指针移到子控件上时
/// 父窗口收不到 WM_MOUSEMOVE 只会收到 WM_MOUSELEAVE，靠本函数区分
/// 「移到滑块上」与「真正离开窗口」。
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

/// 关闭子菜单（先摘除再销毁；主菜单保持打开）。
pub fn close(state: &mut HostState) {
    if let Some(h) = state.sub.take() {
        state.sub_dev = None;
        state.sub_hover_row = None;
        state.sub_pressed = None;
        state.sub_trackbar = None;
        state.sub_wheel_trackbar = None;
        state.sub_scroll_trackbar = None;
        state.model.sub_slider = None;
        state.model.sub_wheel = None;
        state.model.sub_scroll = None;
        state.model.capturing = false;
        state.model.sub_pointer_inside = false;
        unsafe {
            let _ = win::DestroyWindow(h);
        }
    }
}

/// 子菜单窗口过程。GWLP_USERDATA 存宿主 HWND，两跳反查状态
/// （与主菜单一致；WM_DESTROY 不反向访问宿主状态）。
pub unsafe extern "system" fn sub_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
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
            state.model.sub_pointer_inside = true;
            let (x, y) = dip_from_lp(hwnd, lp);
            // 悬停槽位：0..2 = 操作行，3 = 「设为规则」按钮，4 = 滚轮模式勾选，
            // 5 = 鼠标触发键，6 = 键盘触发键
            let mut slot = sub_row_at(y).or_else(|| sub_btn_at(x, y).then_some(3));
            if slot.is_none() && sub_scroll_mode_at(x, y) {
                slot = Some(4);
            }
            if slot.is_none() && sub_scroll_trigger_at(x, y) {
                slot = Some(5);
            }
            if slot.is_none() && sub_kb_trigger_at(x, y) {
                slot = Some(6);
            }
            if slot != state.sub_hover_row {
                state.sub_hover_row = slot;
                let _ = InvalidateRect(Some(hwnd), None, false);
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
            state.sub_pressed = None;
            // 指针可能只是移到了子控件（滑块）上：光标仍在窗口矩形内，
            // 保持打开。不在此处重挂 TrackMouseEvent（重挂会在子控件上
            // 来回触发 leave）；后续由滑块子类的 leave 通知或父窗口
            // WM_MOUSEMOVE（重新登记跟踪）接管。
            if cursor_inside_window(hwnd) {
                return LRESULT(0);
            }
            state.model.sub_pointer_inside = false;
            state.sub_hover_row = None;
            let _ = InvalidateRect(Some(hwnd), None, false);
            // 指针离开子菜单：若不在主菜单设备行/其他设备入口或列表内，
            // 请求异步关闭。处理时重新检查悬停状态。
            let keep = matches!(state.hover, Some(Hover::Device(_) | Hover::OtherDevices))
                || state.model.other_pointer_inside;
            if !keep {
                state.model.sub_hover = None;
                let _ = PostMessageW(Some(host), WM_APP_CLOSE_SUBMENU, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // 按下反馈：只在可用操作上登记按压态
            let state = &mut *ptr;
            let (x, y) = dip_from_lp(hwnd, lp);
            let mut slot = sub_row_at(y).or_else(|| sub_btn_at(x, y).then_some(3));
            if slot.is_none() && sub_scroll_mode_at(x, y) {
                slot = Some(4);
            }
            if slot.is_none() && sub_scroll_trigger_at(x, y) {
                slot = Some(5);
            }
            if slot.is_none() && sub_kb_trigger_at(x, y) {
                slot = Some(6);
            }
            let enabled = match (state.sub_dev, slot) {
                (Some(dev), Some(s)) => {
                    let d = state.model.devs.get(dev);
                    match s {
                        3 => d.map(|d| d.can_rule()).unwrap_or(false),
                        4 | 5 | 6 => d.map(|d| d.can_rule()).unwrap_or(false),
                        _ => d.map(|d| d.sub_actions()[s].1).unwrap_or(false),
                    }
                }
                _ => false,
            };
            state.sub_pressed = if enabled { slot } else { None };
            if state.sub_pressed.is_some() {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            // 子菜单滑块：所有通知都只更新本地预览，绝不直接改系统速度
            let state = &mut *ptr;
            if state.sub_trackbar.map(|h| h.0 as isize) == Some(lp.0) {
                let pos = unsafe {
                    SendMessageW(
                        state.sub_trackbar.unwrap(),
                        TBM_GETPOS,
                        Some(WPARAM(0)),
                        Some(LPARAM(0)),
                    )
                    .0
                } as i32;
                state.model.preview_sub_slider(pos);
                let _ = InvalidateRect(Some(hwnd), None, false);
            } else if state.sub_wheel_trackbar.map(|h| h.0 as isize) == Some(lp.0) {
                let pos = unsafe {
                    SendMessageW(
                        state.sub_wheel_trackbar.unwrap(),
                        TBM_GETPOS,
                        Some(WPARAM(0)),
                        Some(LPARAM(0)),
                    )
                    .0
                } as i32;
                state.model.preview_sub_wheel(pos);
                let _ = InvalidateRect(Some(hwnd), None, false);
            } else if state.sub_scroll_trackbar.map(|h| h.0 as isize) == Some(lp.0) {
                let pos = unsafe {
                    SendMessageW(
                        state.sub_scroll_trackbar.unwrap(),
                        TBM_GETPOS,
                        Some(WPARAM(0)),
                        Some(LPARAM(0)),
                    )
                    .0
                } as i32;
                state.model.preview_sub_scroll_px(pos);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // 松手且未移出按下的槽位才触发（按下反馈语义）
            let state = &mut *ptr;
            let (x, y) = dip_from_lp(hwnd, lp);
            let mut slot = sub_row_at(y).or_else(|| sub_btn_at(x, y).then_some(3));
            if slot.is_none() && sub_scroll_mode_at(x, y) {
                slot = Some(4);
            }
            if slot.is_none() && sub_scroll_trigger_at(x, y) {
                slot = Some(5);
            }
            if slot.is_none() && sub_kb_trigger_at(x, y) {
                slot = Some(6);
            }
            let pressed = state.sub_pressed.take();
            let _ = InvalidateRect(Some(hwnd), None, false);
            if pressed.is_some() && pressed == slot {
                if let (Some(dev), Some(s)) = (state.sub_dev, slot) {
                    match s {
                        3 => {
                            // 「设为规则」：以滑块预览值写入该设备规则（指针 + 滚轮 + 滚轮模式）
                            if let (Some(sp), Some(wh)) =
                                (state.model.sub_slider, state.model.sub_wheel)
                            {
                                let can = state
                                    .model
                                    .devs
                                    .get(dev)
                                    .map(|d: &DevRow| d.can_rule())
                                    .unwrap_or(false);
                                if can {
                                    super::menu::dispatch(
                                        ptr,
                                        vec![MenuAction::SetRuleWithSpeed(
                                            dev,
                                            sp,
                                            wh,
                                            state.model.sub_scroll.clone(),
                                        )],
                                    );
                                }
                            }
                        }
                        4 => {
                            // 滚轮模式开关
                            if let Some(d) = state.model.devs.get(dev) {
                                if d.can_rule() {
                                    state.model.toggle_sub_scroll();
                                    if let Some(tb) = state.sub_scroll_trackbar {
                                        let on = state
                                            .model
                                            .sub_scroll
                                            .as_ref()
                                            .map_or(false, |s| s.enabled);
                                        unsafe {
                                            let _ = EnableWindow(tb, on);
                                        }
                                    }
                                    let _ = InvalidateRect(Some(hwnd), None, false);
                                }
                            }
                        }
                        5 => {
                            // 鼠标触发键录入：仅在滚轮模式启用时
                            let on = state.model.sub_scroll.as_ref().map_or(false, |s| s.enabled);
                            if on && !state.model.capturing {
                                super::scroll_hook::arm_capture(state);
                                state.model.capturing = true;
                                let _ = InvalidateRect(Some(hwnd), None, false);
                            }
                        }
                        6 => {
                            // 键盘触发键录入：仅在滚轮模式启用时
                            let on = state.model.sub_scroll.as_ref().map_or(false, |s| s.enabled);
                            if on && !state.model.capturing {
                                super::scroll_hook::arm_kb_capture(state);
                                state.model.capturing = true;
                                let _ = InvalidateRect(Some(hwnd), None, false);
                            }
                        }
                        _ => {
                            let enabled = state
                                .model
                                .devs
                                .get(dev)
                                .map(|d: &DevRow| d.sub_actions()[s].1)
                                .unwrap_or(false);
                            if enabled {
                                let action = match s {
                                    0 => MenuAction::SetRule(dev),
                                    1 => MenuAction::DelRule(dev),
                                    _ => MenuAction::Reapply(dev),
                                };
                                super::menu::dispatch(ptr, vec![action]);
                            }
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // Trackbar 自绘（NM_CUSTOMDRAW）
            trackbar_notify(&*ptr, lp).unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
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

        if let Some(dev) = state.sub_dev {
            if let Some(d) = state.model.devs.get(dev) {
                // ── 信息行 ──
                draw_text(
                    mem,
                    &d.sub_info(),
                    PAD,
                    SUB_INFO_TOP + SUB_ROW_H / 2.0,
                    s,
                    pal.gray,
                );

                // ── 指针标签行（拖动中实时显示当前档位）──
                let sp = state.model.sub_slider.unwrap_or(10);
                draw_text(
                    mem,
                    &format!("指针速度: {sp}"),
                    PAD,
                    sub_ptr_label_top() + SUB_ROW_H / 2.0,
                    s,
                    pal.text,
                );

                // ── 滚轮标签行 ──
                let wh = state.model.sub_wheel.unwrap_or(3);
                draw_text(
                    mem,
                    &format!("滚轮速度: {wh}"),
                    PAD,
                    sub_wheel_label_top() + SUB_ROW_H / 2.0,
                    s,
                    pal.text,
                );

                // ── 滚轮模式勾选行 ──
                let scroll = state.model.sub_scroll.as_ref();
                let scroll_enabled = scroll.map_or(false, |s| s.enabled);
                let mode_hovered = d.can_rule() && state.sub_hover_row == Some(4);
                let mode_pressed = d.can_rule() && state.sub_pressed == Some(4);
                let mode_top = sub_scroll_mode_top();
                if mode_hovered || mode_pressed {
                    let fill = if mode_pressed {
                        pal.row_pressed
                    } else {
                        pal.highlight
                    };
                    let rr = RECT {
                        left: (1.0 * s).round() as i32,
                        top: (mode_top * s).round() as i32,
                        right: (((SUB_W - 1.0) * s).round() as i32).min(w),
                        bottom: ((mode_top + SUB_ROW_H) * s).round() as i32,
                    };
                    FillRect(mem, &rr, CreateSolidBrush(fill));
                }
                if scroll_enabled {
                    draw_check(mem, mode_top + SUB_ROW_H / 2.0, s, pal.text);
                }
                let mode_color = if !d.can_rule() {
                    pal.gray
                } else if mode_hovered || mode_pressed {
                    pal.hl_text
                } else {
                    pal.text
                };
                draw_text(
                    mem,
                    "滚轮模式",
                    PAD + 18.0,
                    mode_top + SUB_ROW_H / 2.0,
                    s,
                    mode_color,
                );

                // ── 触发键行 ──
                let trigger_top = sub_scroll_trigger_top();
                let trig_hovered = d.can_rule() && scroll_enabled && state.sub_hover_row == Some(5);
                let trig_pressed = d.can_rule() && scroll_enabled && state.sub_pressed == Some(5);
                let trig_color = if !d.can_rule() || !scroll_enabled {
                    pal.gray
                } else if trig_hovered || trig_pressed {
                    pal.hl_text
                } else {
                    pal.text
                };
                if trig_hovered || trig_pressed {
                    let fill = if trig_pressed {
                        pal.row_pressed
                    } else {
                        pal.highlight
                    };
                    let rr = RECT {
                        left: (1.0 * s).round() as i32,
                        top: (trigger_top * s).round() as i32,
                        right: (((SUB_W - 1.0) * s).round() as i32).min(w),
                        bottom: ((trigger_top + SUB_ROW_H) * s).round() as i32,
                    };
                    FillRect(mem, &rr, CreateSolidBrush(fill));
                }
                let kb_capturing = super::scroll_hook::is_kb_capturing();
                let trig_text = if state.model.capturing && !kb_capturing {
                    "触发键: 按下任意鼠标键…(Esc取消)".to_string()
                } else {
                    let t = scroll.map_or(crate::scroll::TriggerBtn::X1, |s| s.trigger);
                    format!("触发键: {}", t.label())
                };
                draw_text(
                    mem,
                    &trig_text,
                    PAD,
                    trigger_top + SUB_ROW_H / 2.0,
                    s,
                    trig_color,
                );

                // ── 键盘触发键行 ──
                let kb_top = sub_kb_trigger_top();
                let kb_hovered = d.can_rule() && scroll_enabled && state.sub_hover_row == Some(6);
                let kb_pressed = d.can_rule() && scroll_enabled && state.sub_pressed == Some(6);
                let kb_color = if !d.can_rule() || !scroll_enabled {
                    pal.gray
                } else if kb_hovered || kb_pressed {
                    pal.hl_text
                } else {
                    pal.text
                };
                if kb_hovered || kb_pressed {
                    let fill = if kb_pressed {
                        pal.row_pressed
                    } else {
                        pal.highlight
                    };
                    let rr = RECT {
                        left: (1.0 * s).round() as i32,
                        top: (kb_top * s).round() as i32,
                        right: (((SUB_W - 1.0) * s).round() as i32).min(w),
                        bottom: ((kb_top + SUB_ROW_H) * s).round() as i32,
                    };
                    FillRect(mem, &rr, CreateSolidBrush(fill));
                }
                let kb_text = if state.model.capturing && kb_capturing {
                    "键盘触发: 按下单个键…(Esc取消)".to_string()
                } else {
                    match scroll.and_then(|s| s.kb_trigger) {
                        Some(t) => format!("键盘触发: {}", t.label()),
                        None => "键盘触发: 无".to_string(),
                    }
                };
                draw_text(
                    mem,
                    &kb_text,
                    PAD,
                    kb_top + SUB_ROW_H / 2.0,
                    s,
                    kb_color,
                );

                // ── 滚动灵敏度标签 ──
                let sens_top = sub_scroll_sens_label_top();
                let sens_px = scroll.map_or(SCROLL_PX_DEFAULT, |s| s.px_per_line);
                let sens_color = if !scroll_enabled { pal.gray } else { pal.text };
                draw_text(
                    mem,
                    &format!("滚动灵敏度: {sens_px} 像素/行"),
                    PAD,
                    sens_top + SUB_ROW_H / 2.0,
                    s,
                    sens_color,
                );

                // ── 「设为规则」按钮（整行）──
                let btn_hovered = d.can_rule() && state.sub_hover_row == Some(3);
                let btn_pressed = d.can_rule() && state.sub_pressed == Some(3);
                let (bl, bt, br, bb) = sub_btn_rect();
                let btn = RECT {
                    left: ((bl + 0.5) * s).round() as i32,
                    top: ((bt + 0.5) * s).round() as i32,
                    right: ((br - 0.5) * s).round() as i32,
                    bottom: ((bb - 0.5) * s).round() as i32,
                };
                if btn_pressed {
                    FillRect(mem, &btn, CreateSolidBrush(pal.btn_pressed));
                } else if btn_hovered {
                    FillRect(mem, &btn, CreateSolidBrush(pal.btn_hover));
                }
                let pen = CreatePen(PS_SOLID, 1, pal.border);
                let old_pen = SelectObject(mem, pen.into());
                // 同 menu.rs：NULL_BRUSH 防止 Rectangle 用白刷盖掉悬停底色
                let old_brush = SelectObject(
                    mem,
                    windows::Win32::Graphics::Gdi::GetStockObject(
                        windows::Win32::Graphics::Gdi::NULL_BRUSH,
                    )
                    .into(),
                );
                let _ = windows::Win32::Graphics::Gdi::Rectangle(
                    mem, btn.left, btn.top, btn.right, btn.bottom,
                );
                SelectObject(mem, old_brush);
                SelectObject(mem, old_pen);
                let _ = DeleteObject(pen.into());
                let btn_color = if !d.can_rule() {
                    pal.gray
                } else if btn_hovered || btn_pressed {
                    pal.hl_text
                } else {
                    pal.text
                };
                draw_text_center(mem, "设为规则", bl, bt, br, bb, s, btn_color);

                // ── 操作行 ──
                for (slot, (label, enabled)) in d.sub_actions().into_iter().enumerate() {
                    let top = sub_action_top(slot);
                    let hovered = enabled && state.sub_hover_row == Some(slot);
                    let pressed = enabled && state.sub_pressed == Some(slot);
                    let color = if hovered || pressed {
                        pal.hl_text
                    } else if !enabled {
                        pal.gray
                    } else {
                        pal.text
                    };
                    if hovered || pressed {
                        let fill = if pressed {
                            pal.row_pressed
                        } else {
                            pal.highlight
                        };
                        let rr = RECT {
                            left: (1.0 * s).round() as i32,
                            top: (top * s).round() as i32,
                            right: (((SUB_W - 1.0) * s).round() as i32).min(w),
                            bottom: ((top + SUB_ROW_H) * s).round() as i32,
                        };
                        FillRect(mem, &rr, CreateSolidBrush(fill));
                    }
                    draw_text(mem, label, PAD, top + SUB_ROW_H / 2.0, s, color);
                }
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
