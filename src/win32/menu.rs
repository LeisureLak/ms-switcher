//! 原生主菜单窗口：自绘行 + 真 Trackbar 子控件。
//!
//! - `WS_POPUP | WS_EX_TOOLWINDOW`：不进任务栏/Alt+Tab，owner 为宿主窗口。
//! - 绘制走内存 DC 双缓冲（WM_ERASEBKGND 返回 1），避免逐行重绘闪烁。
//! - 布局与命中判定全部来自 [`crate::menu_model`] 的 DIP 常量/纯函数；
//!   本模块只做 px↔DIP 换算、GDI 绘制与消息 → 模型事件映射。
//! - 滑块是 `msctls_trackbar32` 子控件：鼠标拖动与方向键调速开箱即用，
//!   并子类化转发 Esc/Q（踩坑 三-4 的对应处理：child 控件不影响主菜单
//!   的 WM_ACTIVATE 失焦关闭逻辑）。
//!
//! 重入纪律（踩坑 三-1/三-3）：动作执行里可能经宿主 close_menu 销毁本窗口，
//! 因此消息处理不长期持有 `&mut HostState` 跨 DestroyWindow；关闭只经
//! `PostMessageW(WM_APP_CLOSE_MENU)` 或「先摘除再销毁」的宿主侧 close_menu。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateFontW, CreatePen, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawFocusRect, DrawTextW,
    Ellipse, EndPaint, FF_DONTCARE, FW_NORMAL, FillRect, FrameRect, GetStockObject, HDC, HFONT,
    InvalidateRect, LineTo, MoveToEx, NULL_BRUSH, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    PS_SOLID, RoundRect, SRCCOPY, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT,
    CDIS_DISABLED, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, NMCUSTOMDRAW,
    NMHDR, NM_CUSTOMDRAW, SetWindowTheme, TBM_SETPOS, TBM_SETRANGE, TBCD_CHANNEL, TBCD_THUMB,
    TBS_HORZ, TBS_NOTICKS, TRACKBAR_CLASS, WM_MOUSELEAVE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    IsWindowEnabled, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE,
    VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging as win;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, GetClientRect,
    GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, HMENU,
    IDC_ARROW, LoadCursorW, PostMessageW, RegisterClassExW, SW_SHOW, SWP_NOZORDER, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_CHAR, WM_ERASEBKGND, WM_HSCROLL, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NOTIFY, WM_PAINT, WNDCLASSEXW,
    WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::menu_model::{
    self, CHECK_W, EFF_INFO_H, EffectiveInfo, Hover, INFO_ROW_H, MENU_W, MenuAction, PAD, ROW_H,
    RULE_ICON_W, SEP_H, SLIDER_H, TITLE_H, TOP_PAD, autostart_row_top, device_row_top,
    eff_row_top, exit_row_top, hover_at, menu_height, other_entry_text, other_row_top,
    reset_btn_rect, wheel_label_top, wheel_slider_top,
};
use crate::scroll::{SCROLL_PX_DEFAULT, TriggerBtn};

use super::host::{HostState, WM_APP_CLOSE_MENU, sync_tip};

/// TBM_GETPOS 未包含在 windows crate 绑定中，值为 WM_USER。
const TBM_GETPOS: u32 = win::WM_USER;

/// Trackbar 经 WM_HSCROLL 的通知码（wParam 低字）。
const TB_LINEUP: u32 = 0;
const TB_LINEDOWN: u32 = 1;
const TB_PAGEUP: u32 = 2;
const TB_PAGEDOWN: u32 = 3;
const TB_THUMBPOSITION: u32 = 4;
const TB_THUMBTRACK: u32 = 5;
const TB_TOP: u32 = 6;
const TB_BOTTOM: u32 = 7;
const TB_ENDTRACK: u32 = 8;

const MENU_CLASS: windows::core::PCWSTR = w!("MSS_Menu");
pub(crate) const SUB_CLASS: windows::core::PCWSTR = w!("MSS_SubMenu");

// ── 颜色（与 egui 版一致）─────────────────────────────
pub(crate) const BG: COLORREF = COLORREF(0x00F0_F0F0);
pub(crate) const BORDER: COLORREF = COLORREF(0x009A_9A9A);
const SEPARATOR: COLORREF = COLORREF(0x00D9_D9D9);
pub(crate) const TEXT: COLORREF = COLORREF(0x001F_1F1F);
pub(crate) const GRAY: COLORREF = COLORREF(0x006D_6D6D);
// COLORREF 字节序为 0x00BBGGRR：蓝色 #0078D7 → 0x00D77800
pub(crate) const HIGHLIGHT: COLORREF = COLORREF(0x00D7_7800);
pub(crate) const HIGHLIGHT_TEXT: COLORREF = COLORREF(0x00FF_FFFF);

// ── 主题调色板 ────────────────────────────────────────
// 跟随系统暗色主题；高对比度模式下整体改用系统菜单色。

/// 当前调色板（按暗色标志取一套；高对比度时由 host 侧直接给系统色）。
#[derive(Clone, Copy)]
pub(crate) struct Pal {
    pub bg: COLORREF,
    pub border: COLORREF,
    pub sep: COLORREF,
    pub text: COLORREF,
    pub gray: COLORREF,
    pub highlight: COLORREF,
    /// 悬停中的行/按钮按压色（比 highlight 深一档的按下反馈）。
    pub row_pressed: COLORREF,
    pub hl_text: COLORREF,
    pub btn_hover: COLORREF,
    pub btn_pressed: COLORREF,
}

pub(crate) fn palette(dark: bool) -> Pal {
    if dark {
        Pal {
            bg: COLORREF(0x002C_2C2C),
            border: COLORREF(0x005A_5A5A),
            sep: COLORREF(0x003F_3F3F),
            text: COLORREF(0x00F3_F3F3),
            gray: COLORREF(0x00A0_A0A0),
            highlight: HIGHLIGHT,
            row_pressed: COLORREF(0x0077_4500), // #004577，比 #0078D7 深一档
            hl_text: HIGHLIGHT_TEXT,
            // 按钮悬停/按下与行高亮同色系：灰阶差只有 ~6%，肉眼不可辨
            btn_hover: HIGHLIGHT,
            btn_pressed: COLORREF(0x0077_4500), // #004577
        }
    } else {
        Pal {
            bg: BG,
            border: BORDER,
            sep: SEPARATOR,
            text: TEXT,
            gray: GRAY,
            highlight: HIGHLIGHT,
            row_pressed: COLORREF(0x009E_5A00), // #005A9E
            hl_text: HIGHLIGHT_TEXT,
            btn_hover: HIGHLIGHT,
            btn_pressed: COLORREF(0x009E_5A00), // #005A9E
        }
    }
}

/// 高对比度：全部取系统菜单色，保证可辨认。
pub(crate) fn hc_pal() -> Pal {
    use windows::Win32::Graphics::Gdi::GetSysColor;
    unsafe {
        let sc = |i: windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX| COLORREF(GetSysColor(i));
        Pal {
            bg: sc(windows::Win32::Graphics::Gdi::COLOR_MENU),
            border: sc(windows::Win32::Graphics::Gdi::COLOR_WINDOWFRAME),
            sep: sc(windows::Win32::Graphics::Gdi::COLOR_GRAYTEXT),
            text: sc(windows::Win32::Graphics::Gdi::COLOR_MENUTEXT),
            gray: sc(windows::Win32::Graphics::Gdi::COLOR_GRAYTEXT),
            highlight: sc(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHT),
            row_pressed: sc(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHT),
            hl_text: sc(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHTTEXT),
            btn_hover: sc(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHT),
            btn_pressed: sc(windows::Win32::Graphics::Gdi::COLOR_GRAYTEXT),
        }
    }
}

/// 系统「应用使用深色模式」？（注册表 AppsUseLightTheme；缺省浅色）
pub(crate) fn system_dark() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut v: u32 = 1;
    let mut size = 4u32;
    let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = w!("AppsUseLightTheme");
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut c_void),
            Some(&mut size),
        );
    }
    v == 0
}

/// DWM 现代外观：Win11 圆角小圆角 + 边框色；Win10 属性不支持时静默降级。
pub(crate) fn apply_dwm(hwnd: HWND, pal: &Pal) {
    use windows::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUNDSMALL,
        DwmSetWindowAttribute,
    };
    unsafe {
        let pref: u32 = DWMWCP_ROUNDSMALL.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const u32 as *const c_void,
            4,
        );
        let border: u32 = pal.border.0;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border as *const u32 as *const c_void,
            4,
        );
    }
}

/// 给 Trackbar 子控件应用当前主题（暗色/高对比度/浅色）。
/// 由于 UxTheme 不保证 msctls_trackbar32 有暗色变体，这里只做主题重置并触发重绘；
/// 真正的绘制由父窗口的 `NM_CUSTOMDRAW` 自绘接管。
pub(crate) fn trackbar_theme(tb: HWND, _dark: bool, _hc: bool) {
    unsafe {
        let _ = SetWindowTheme(tb, None::<&PCWSTR>, None::<&PCWSTR>);
        let _ = SendMessageW(tb, win::WM_THEMECHANGED, Some(WPARAM(0)), Some(LPARAM(0)));
    }
}

/// Trackbar 自绘：暗色/浅色/高对比度下都由我们绘制通道和滑块，
/// 这样就不用依赖 UxTheme 是否给 msctls_trackbar32 提供了暗色变体。
pub(crate) unsafe fn trackbar_custom_draw(pal: &Pal, nmc: &NMCUSTOMDRAW) -> LRESULT {
    if nmc.dwDrawStage == CDDS_PREPAINT {
        // CDDS_PREPAINT 的 rc 为空，需要自己取客户区并把整个控件背景填成菜单背景色
        let mut client = RECT::default();
        let _ = GetClientRect(nmc.hdr.hwndFrom, &mut client);
        let bg = CreateSolidBrush(pal.bg);
        let _ = FillRect(nmc.hdc, &client, bg);
        let _ = DeleteObject(bg.into());
        return LRESULT((CDRF_NOTIFYITEMDRAW | CDRF_SKIPDEFAULT) as isize);
    }
    if nmc.dwDrawStage == CDDS_ITEMPREPAINT {
        let hdc = nmc.hdc;
        let rc = nmc.rc;
        let enabled = IsWindowEnabled(nmc.hdr.hwndFrom).as_bool();
        let disabled = nmc.uItemState.contains(CDIS_DISABLED);

        if nmc.dwItemSpec == TBCD_CHANNEL as usize {
            let brush = CreateSolidBrush(pal.border);
            let null_pen = GetStockObject(NULL_PEN);
            let old_pen = SelectObject(hdc, null_pen);
            let old_brush = SelectObject(hdc, brush.into());
            let h = rc.bottom - rc.top;
            let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, h, h);
            let _ = SelectObject(hdc, old_brush);
            let _ = SelectObject(hdc, old_pen);
            let _ = DeleteObject(brush.into());
        } else if nmc.dwItemSpec == TBCD_THUMB as usize {
            let thumb = if enabled && !disabled { pal.highlight } else { pal.gray };
            let brush = CreateSolidBrush(thumb);
            let null_pen = GetStockObject(NULL_PEN);
            let old_pen = SelectObject(hdc, null_pen);
            let old_brush = SelectObject(hdc, brush.into());
            let _ = Ellipse(hdc, rc.left, rc.top, rc.right, rc.bottom);
            let _ = SelectObject(hdc, old_brush);
            let _ = SelectObject(hdc, old_pen);
            let _ = DeleteObject(brush.into());
        }
        return LRESULT(CDRF_SKIPDEFAULT as isize);
    }
    LRESULT(CDRF_DODEFAULT as isize)
}

/// 如果 WM_NOTIFY 来自某个 Trackbar 的 NM_CUSTOMDRAW，返回自绘结果。
pub(crate) unsafe fn trackbar_notify(state: &HostState, lp: LPARAM) -> Option<LRESULT> {
    let nm = &*(lp.0 as *const NMHDR);
    if nm.code != NM_CUSTOMDRAW {
        return None;
    }
    let tbs = [
        state.trackbar,
        state.wheel_trackbar,
        state.sub_trackbar,
        state.sub_wheel_trackbar,
        state.sub_scroll_trackbar,
    ];
    if tbs.iter().any(|h| h.map(|x| x == nm.hwndFrom).unwrap_or(false)) {
        let nmc = &*(lp.0 as *const NMCUSTOMDRAW);
        Some(trackbar_custom_draw(&current_pal(state), nmc))
    } else {
        None
    }
}

/// 绘制用调色板：高对比度优先，否则按主题暗色标志。
pub(crate) fn current_pal(state: &HostState) -> Pal {
    if state.hc {
        hc_pal()
    } else {
        palette(state.theme_dark)
    }
}

/// 是否高对比度模式。
pub(crate) fn high_contrast() -> bool {
    use windows::Win32::UI::Accessibility::HIGHCONTRASTW;
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    let mut hc = HIGHCONTRASTW::default();
    hc.cbSize = std::mem::size_of::<HIGHCONTRASTW>() as u32;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some(&mut hc as *mut HIGHCONTRASTW as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    (hc.dwFlags.0 & 1) != 0 // HCF_HIGHCONTRASTON
}

/// 按窗口 DPI 创建菜单 UI 字体（13 逻辑像素，含中文回退）。
/// GDI 字体高度是物理像素，必须随 DPI 缩放；字体随菜单关闭销毁（见 close）。
pub(crate) fn menu_font_for(dpi: u32) -> HFONT {
    let h_px = -(13 * dpi as i32 / 96).max(8);
    unsafe {
        CreateFontW(
            h_px,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            w!("Microsoft YaHei UI"),
        )
    }
}

/// 当前菜单字体（宿主状态持有，open 时按窗口 DPI 创建）。
pub(crate) fn ui_font(state: &HostState) -> HFONT {
    state.font
}

/// 注册菜单/子菜单窗口类 + 初始化 Common Controls（进程内一次）。
pub fn register_class(hinstance: HINSTANCE) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(menu_wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: MENU_CLASS,
            ..Default::default()
        };
        assert_ne!(RegisterClassExW(&wc), 0, "register menu class");

        let wcsub = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(super::submenu::sub_wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: SUB_CLASS,
            ..Default::default()
        };
        assert_ne!(RegisterClassExW(&wcsub), 0, "register submenu class");

        super::other_submenu::register_class(hinstance);

        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
    });
}

/// 窗口 DPI → 缩放系数。
pub(crate) fn dpi_scale(hwnd: HWND) -> f32 {
    (unsafe { GetDpiForWindow(hwnd) }) as f32 / 96.0
}

/// DIP → 物理像素。
fn px(hwnd: HWND, dip: f32) -> i32 {
    (dip * dpi_scale(hwnd)).round() as i32
}

/// 打开菜单：刷新模型数据、按真实 DPI 定尺寸、光标处弹出并置于前台。
pub fn open(state: &mut HostState) -> Result<HWND, windows::core::Error> {
    // 数据刷新（与旧版 open_menu 一致）
    let mice = crate::devices::enumerate_mice();
    state.app.apply_diff(&mice);
    state.model
        .set_devs(menu_model::build_dev_rows(&mice, &state.app));
    state.model.effective = menu_model::effective_info(&state.app);
    state.model.speed_val = crate::speed::get();
    state.model.wheel_val = crate::speed::get_wheel();
    state.model.pending_speed = None;
    state.model.pending_wheel = None;
    state.model.sub_slider = None;
    state.model.sub_wheel = None;
    state.model.sub_scroll = None;
    state.model.autostart_on = crate::autostart::is_enabled();
    state.model.capturing = false;
    state.model.kb_focus = None;
    state.hover = None;

    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    register_class(hinstance);

    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }

    // 两阶段开窗：先在光标处以 0 尺寸创建，拿到窗口真实 DPI 后再定位/定尺寸
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            MENU_CLASS,
            w!("MSS Menu"),
            WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            pt.x,
            pt.y,
            0,
            0,
            Some(state.hwnd), // owner：不进任务栏/Alt+Tab
            None,
            Some(hinstance.into()),
            Some(state.hwnd.0.cast()),
        )?
    };

    let s = dpi_scale(hwnd);
    if state.debug {
        eprintln!("[mss-debug] open: menu dpi={}", unsafe {
            GetDpiForWindow(hwnd)
        });
    }
    let h_dip = menu_height(state.model.ruled_devs.len());
    // 主菜单按自身宽度夹取；子菜单放不下时由 submenu 自行翻转到左缘
    let (x, y) = clamp_to_work_area(pt, MENU_W * s, h_dip * s);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(win::HWND_TOPMOST),
            x,
            y,
            (MENU_W * s).round() as i32,
            (h_dip * s).round() as i32,
            SWP_NOZORDER,
        );
    }

    apply_dwm(hwnd, &current_pal(state));

    // 菜单字体：按本窗口 DPI 创建（宿主状态持有，关闭时销毁）
    if state.font.0.is_null() {
        state.font = menu_font_for(unsafe { GetDpiForWindow(hwnd) });
    }

    // 滑块子控件（指针 1–20 / 滚轮 1–100）
    let tb = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            TRACKBAR_CLASS,
            w!(""),
            WS_CHILD
                | WS_VISIBLE
                | WINDOW_STYLE(TBS_HORZ | TBS_NOTICKS),
            px(hwnd, PAD),
            px(hwnd, TOP_PAD + TITLE_H),
            px(hwnd, MENU_W - 2.0 * PAD),
            px(hwnd, SLIDER_H),
            Some(hwnd),
            Some(HMENU(1 as *mut c_void)),
            Some(hinstance.into()),
            None,
        )?
    };
    let tb_wheel = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            TRACKBAR_CLASS,
            w!(""),
            WS_CHILD
                | WS_VISIBLE
                | WINDOW_STYLE(TBS_HORZ | TBS_NOTICKS),
            px(hwnd, PAD),
            px(hwnd, wheel_slider_top()),
            px(hwnd, MENU_W - 2.0 * PAD),
            px(hwnd, SLIDER_H),
            Some(hwnd),
            Some(HMENU(2 as *mut c_void)),
            Some(hinstance.into()),
            None,
        )?
    };
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
            Some(LPARAM(state.model.speed_val as isize)),
        );
        let _ = SetWindowSubclass(tb, Some(trackbar_proc), 1, state.hwnd.0 as usize);
        SendMessageW(
            tb_wheel,
            TBM_SETRANGE,
            Some(WPARAM(0)),
            Some(LPARAM(makelong(1, 100) as isize)),
        );
        SendMessageW(
            tb_wheel,
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(state.model.wheel_val as isize)),
        );
        let _ = SetWindowSubclass(tb_wheel, Some(trackbar_proc), 2, state.hwnd.0 as usize);

        trackbar_theme(tb, state.theme_dark, state.hc);
        trackbar_theme(tb_wheel, state.theme_dark, state.hc);
    }
    state.trackbar = Some(tb);
    state.wheel_trackbar = Some(tb_wheel);

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        force_foreground(hwnd);
    }
    Ok(hwnd)
}

/// 把菜单窗口推到前台并激活。
///
/// 从托盘打开弹窗时 Windows 的前台锁常让 `SetForegroundWindow` 静默失败——
/// 菜单看似在前台实际从未激活，之后点击菜单外收不到 WM_ACTIVATE(WA_INACTIVE)，
/// 失焦关闭就永远不触发。这里用标准的 AttachThreadInput 技巧：临时附加到
/// 当前前台线程的输入队列再抢前台。
fn force_foreground(hwnd: HWND) {
    unsafe {
        let cur = GetCurrentThreadId();
        let fg = GetForegroundWindow();
        let fg_thread = if fg.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        let attached =
            fg_thread != 0 && fg_thread != cur && AttachThreadInput(fg_thread, cur, true).as_bool();
        let ok = SetForegroundWindow(hwnd).as_bool();
        let _ = SetFocus(Some(hwnd));
        if attached {
            let _ = AttachThreadInput(fg_thread, cur, false);
        }
        if !ok {
            eprintln!("[mss-debug] SetForegroundWindow failed after AttachThreadInput");
        }
    }
}

fn makelong(lo: i32, hi: i32) -> u32 {
    (lo as u16 as u32) | ((hi as u16 as u32) << 16)
}

/// 把光标点夹取到所在显示器工作区，使 w×h（物理像素）的窗口完整可见。
fn clamp_to_work_area(pt: POINT, w: f32, h: f32) -> (i32, i32) {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO::default();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let nx = (pt.x as f32).clamp(
                mi.rcWork.left as f32,
                (mi.rcWork.right as f32 - w).max(mi.rcWork.left as f32),
            );
            let ny = (pt.y as f32).clamp(
                mi.rcWork.top as f32,
                (mi.rcWork.bottom as f32 - h).max(mi.rcWork.top as f32),
            );
            (nx as i32, ny as i32)
        } else {
            (pt.x, pt.y)
        }
    }
}

unsafe extern "system" fn menu_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lp.0 as *const CREATESTRUCTW;
        SetWindowLongPtrW(hwnd, win::GWLP_USERDATA, (*cs).lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    // GWLP_USERDATA 存的是宿主 HWND（open 传入）；宿主的 GWLP_USERDATA
    // 才是 HostState 指针——两跳反查，切勿混用（曾因此段错误）
    let host = HWND(GetWindowLongPtrW(hwnd, win::GWLP_USERDATA) as *mut c_void);
    if host.is_invalid() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ptr = GetWindowLongPtrW(host, win::GWLP_USERDATA) as *mut HostState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }

    match msg {
        WM_ACTIVATE => {
            // 失活只投递关闭请求，绝不同步 DestroyWindow（踩坑 三-3）。
            // 此臂可能在 open 调用栈内到达：只碰原子量，不取 &mut。
            let active = (wp.0 as u16) != 0; // WA_INACTIVE = 0
            let state = &*ptr;
            let host = state.hwnd;
            if active {
                state
                    .menu_ever_active
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            } else if state
                .menu_ever_active
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let _ = PostMessageW(Some(host), WM_APP_CLOSE_MENU, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // 双缓冲自绘，禁系统擦除
        WM_PAINT => {
            paint(ptr, hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let state = &mut *ptr;
            let (x, y) = dip_from_lp(hwnd, lp);
            let n_ruled = state.model.ruled_devs.len();
            let h = hover_at(x, y, n_ruled);
            if state.debug {
                eprintln!(
                    "[mss-debug] menu mm raw={:#x} ({x:.0},{y:.0}) hover={h:?} scale={}",
                    lp.0,
                    dpi_scale(hwnd)
                );
            }
            if h != state.hover {
                state.hover = h;
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            match h {
                Some(Hover::Device(local)) => {
                    state.model.set_other_entry_hover(false);
                    state.model.update_hover_ruled(Some(local));
                    super::other_submenu::sync(state, hwnd);
                    super::submenu::sync(state, hwnd);
                }
                Some(Hover::OtherDevices) => {
                    // 先清掉单设备配置子菜单目标（update_hover_ruled 会顺带
                    // 把 other_entry_hovered 清 false），再显式标记为 other 入口。
                    state.model.update_hover_ruled(None);
                    state.model.set_other_entry_hover(true);
                    super::other_submenu::sync(state, hwnd);
                    super::submenu::sync(state, hwnd);
                }
                _ => {
                    state.model.set_other_entry_hover(false);
                    state.model.update_hover_ruled(None);
                    super::other_submenu::sync(state, hwnd);
                    super::submenu::sync(state, hwnd);
                }
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
            state.pressed = None;
            if state.hover.is_some() {
                state.hover = None;
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            // 鼠标离开主菜单：若光标仍在其他设备列表/单设备配置子菜单内，
            // 保持打开；否则清理悬停状态。
            let inside_other = state
                .other_sub
                .map_or(false, |h| super::other_submenu::cursor_inside_window(h));
            let inside_sub = state
                .sub
                .map_or(false, |h| super::submenu::cursor_inside_window(h));
            if !inside_other && !inside_sub {
                state.model.set_other_entry_hover(false);
                state.model.update_hover_ruled(None);
                super::other_submenu::sync(state, hwnd);
                super::submenu::sync(state, hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // 按下反馈：只在命令区登记按压态，松手且未移出才触发
            let state = &mut *ptr;
            let (x, y) = dip_from_lp(hwnd, lp);
            state.pressed = match hover_at(x, y, state.model.ruled_devs.len()) {
                Some(h @ (Hover::Reset | Hover::Autostart | Hover::Exit)) => Some(h),
                _ => None,
            };
            if state.pressed.is_some() {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let state = &mut *ptr;
            let (x, y) = dip_from_lp(hwnd, lp);
            let pressed = state.pressed.take();
            let fire = pressed.is_some() && pressed == hover_at(x, y, state.model.ruled_devs.len());
            let _ = InvalidateRect(Some(hwnd), None, false);
            if fire {
                let actions = state.model.click_at(x, y);
                dispatch(ptr, actions);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if unsafe { (*ptr).debug } {
                eprintln!("[mss-debug] menu keydown wp={:#x}", wp.0);
            }
            match wp.0 as u32 {
                k if k == VK_ESCAPE.0 as u32 => {
                    if unsafe { (*ptr).debug } {
                        eprintln!("[mss-debug] Esc -> close request");
                    }
                    let _ = PostMessageW(
                        Some(super::host::host_hwnd(ptr)),
                        WM_APP_CLOSE_MENU,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
                k if k == VK_RETURN.0 as u32 || k == VK_SPACE.0 as u32 => {
                    let actions = unsafe { (*ptr).model.kb_activate() };
                    dispatch(ptr, actions);
                }
                k if k == VK_DOWN.0 as u32 => {
                    let state = &mut *ptr;
                    state.model.kb_focus_next();
                    state.hover = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                k if k == VK_UP.0 as u32 => {
                    let state = &mut *ptr;
                    state.model.kb_focus_prev();
                    state.hover = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                k if k == VK_RIGHT.0 as u32 => {
                    let state = &mut *ptr;
                    nudge_speed(state, 1);
                }
                k if k == VK_LEFT.0 as u32 => {
                    let state = &mut *ptr;
                    nudge_speed(state, -1);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // Trackbar 自绘（NM_CUSTOMDRAW）
            trackbar_notify(&*ptr, lp).unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        WM_HSCROLL => {
            // lp = 发送通知的 Trackbar 子控件 HWND；wParam 低字 = 通知码
            let state = &mut *ptr;
            let notify = (wp.0 & 0xFFFF) as u32;
            if state.trackbar.map(|h| h.0 as isize) == Some(lp.0) {
                let pos = unsafe {
                    SendMessageW(
                        state.trackbar.unwrap(),
                        TBM_GETPOS,
                        Some(WPARAM(0)),
                        Some(LPARAM(0)),
                    )
                    .0
                } as i32;
                handle_slider_event(state, notify, pos, false);
            } else if state.wheel_trackbar.map(|h| h.0 as isize) == Some(lp.0) {
                let pos = unsafe {
                    SendMessageW(
                        state.wheel_trackbar.unwrap(),
                        TBM_GETPOS,
                        Some(WPARAM(0)),
                        Some(LPARAM(0)),
                    )
                    .0
                } as i32;
                handle_slider_event(state, notify, pos, true);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 滑块子类：Esc/Q 转发给宿主（滑块获得焦点后主菜单收不到键盘）。
/// 子类 id 3/4/5 = 子菜单的指针/滚轮/滚轮灵敏度滑块：父窗口收不到子控件上的鼠标事件，
/// 需在子控件上单独登记 leave，光标从滑块直接移出子菜单窗口时通知宿主
/// （WM_APP_SUB_LEFT）。
pub(crate) unsafe extern "system" fn trackbar_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    uid: usize,
    data: usize,
) -> LRESULT {
    // 背景由 NM_CUSTOMDRAW 的 CDDS_PREPAINT 阶段填充；
    // 禁止系统默认的浅色/主题背景擦除，避免闪烁。
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    if uid == 3 || uid == 4 || uid == 5 {
        if msg == WM_MOUSEMOVE {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
        }
        if msg == WM_MOUSELEAVE {
            let _ = PostMessageW(
                Some(HWND(data as *mut c_void)),
                super::host::WM_APP_SUB_LEFT,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    if msg == WM_KEYDOWN && wp.0 as u32 == VK_ESCAPE.0 as u32 {
        let _ = PostMessageW(
            Some(HWND(data as *mut c_void)),
            WM_APP_CLOSE_MENU,
            WPARAM(0),
            LPARAM(0),
        );
        return LRESULT(0);
    }
    if msg == WM_CHAR && (wp.0 as u8 == b'q' || wp.0 as u8 == b'Q') {
        let _ = PostMessageW(
            Some(HWND(data as *mut c_void)),
            win::WM_CLOSE,
            WPARAM(0),
            LPARAM(0),
        );
        return LRESULT(0);
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

/// 消息坐标 → 菜单内 DIP 坐标。
pub(crate) fn dip_from_lp(hwnd: HWND, lp: LPARAM) -> (f32, f32) {
    let x = (lp.0 & 0xFFFF) as u16 as i16 as i32;
    let y = ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    let s = dpi_scale(hwnd);
    (x as f32 / s, y as f32 / s)
}

/// 执行模型动作；返回 false 表示菜单应关闭（与 egui 版 run_action 语义一致）。
pub(crate) fn dispatch(ptr: *mut HostState, actions: Vec<MenuAction>) {
    for action in actions {
        let keep = run_action(ptr, action);
        if !keep {
            super::host::close_menu(unsafe { &mut *ptr });
            sync_tip(unsafe { &mut *ptr });
            return;
        }
    }
}

fn run_action(ptr: *mut HostState, action: MenuAction) -> bool {
    let state = unsafe { &mut *ptr };
    match action {
        MenuAction::SetSpeed(v) => {
            speed_set(state, v);
            true
        }
        MenuAction::ResetDefault => {
            // 指针与滚轮都恢复 Windows 默认；speed_set/wheel_set 负责模型、
            // 滑块位置与 tooltip 的同步
            speed_set(state, crate::speed::SPEED_DEFAULT);
            wheel_set(state, crate::speed::WHEEL_DEFAULT);
            true
        }
        MenuAction::ToggleAutostart => {
            if crate::autostart::is_enabled() {
                crate::autostart::disable();
            } else {
                crate::autostart::enable();
            }
            state.model.autostart_on = crate::autostart::is_enabled();
            invalidate_state(state);
            true // 不关闭菜单（与旧版一致）
        }
        MenuAction::SetRule(i) => {
            if let Some(d) = state.model.devs.get(i) {
                if let (Some(v), Some(p)) = (d.vid.clone(), d.pid.clone()) {
                    // 「用当前速度保存规则」：指针与滚轮都取当前系统值，滚轮模式保留旧值
                    let cur = crate::speed::get();
                    let wheel = crate::speed::get_wheel();
                    let scroll = state
                        .app
                        .cfg
                        .rule_for(&v, &p)
                        .and_then(|r| r.scroll.clone());
                    state
                        .app
                        .cfg
                        .set_rule(&v, &p, cur, Some(wheel), scroll, Some(d.name.clone()));
                    let _ = crate::config::save(&state.app.cfg);
                    state.app.reapply();
                    super::scroll_hook::sync(state);
                }
            }
            false
        }
        MenuAction::SetRuleWithSpeed(i, sp, wh, scroll) => {
            if let Some(d) = state.model.devs.get(i) {
                if let (Some(v), Some(p)) = (d.vid.clone(), d.pid.clone()) {
                    // 「设为规则」按钮：指针、滚轮、滚轮模式都取子菜单本地值
                    state
                        .app
                        .cfg
                        .set_rule(&v, &p, sp, Some(wh), scroll, Some(d.name.clone()));
                    let _ = crate::config::save(&state.app.cfg);
                    state.app.reapply();
                    super::scroll_hook::sync(state);
                }
            }
            false
        }
        MenuAction::DelRule(i) => {
            if let Some(d) = state.model.devs.get(i) {
                if let (Some(v), Some(p)) = (d.vid.clone(), d.pid.clone()) {
                    state.app.cfg.remove_rule(&v, &p);
                    let _ = crate::config::save(&state.app.cfg);
                    state.app.reapply();
                    super::scroll_hook::sync(state);
                }
            }
            false
        }
        MenuAction::Reapply(_) => {
            state.app.reapply();
            super::scroll_hook::sync(state);
            false
        }
        MenuAction::Exit => {
            let host = state.hwnd;
            unsafe {
                let _ = PostMessageW(Some(host), win::WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            false
        }
    }
}

/// 滑块事件分流（需求：拖动只预览，松手才应用）。
///
/// - `TB_THUMBTRACK`（拖动中）：只更新标题预览，不碰系统值；
/// - `TB_THUMBPOSITION` / `TB_ENDTRACK`（松手）：提交预览值；
/// - `TB_LINEUP`/`LINEDOWN`/`PAGEUP`/`PAGEDOWN`/`TOP`/`BOTTOM`（方向键、
///   点击滑轨等离散操作）：逐次直接应用。
/// `wheel = true` 时作用于滚轮滑块（1–100），否则指针滑块（1–20）。
fn handle_slider_event(state: &mut HostState, notify: u32, pos: i32, wheel: bool) {
    match notify {
        TB_THUMBTRACK => {
            if wheel {
                state.model.preview_wheel(pos);
            } else {
                state.model.preview_speed(pos);
            }
            invalidate_state(state);
        }
        TB_THUMBPOSITION | TB_ENDTRACK => {
            let committed = if wheel {
                state.model.commit_wheel()
            } else {
                state.model.commit_speed()
            };
            if let Some(v) = committed {
                if wheel {
                    wheel_set(state, v);
                } else {
                    speed_set(state, v);
                }
            } else {
                // 拖回原值松手：清预览、恢复标题即可
                if wheel {
                    state.model.pending_wheel = None;
                } else {
                    state.model.pending_speed = None;
                }
                invalidate_state(state);
            }
        }
        TB_LINEUP | TB_LINEDOWN | TB_PAGEUP | TB_PAGEDOWN | TB_TOP | TB_BOTTOM => {
            let (min, max) = if wheel { (1, 100) } else { (1, 20) };
            let v = pos.clamp(min, max) as u32;
            if wheel {
                state.model.pending_wheel = None;
                if v != state.model.wheel_val {
                    wheel_set(state, v);
                }
            } else {
                state.model.pending_speed = None;
                if v != state.model.speed_val {
                    speed_set(state, v);
                }
            }
        }
        _ => {}
    }
}

/// 指针速度落盘 + 模型 + 滑块 + tooltip 的单一入口。
fn speed_set(state: &mut HostState, v: u32) {
    crate::speed::set(v);
    state.model.speed_val = v.clamp(1, 20);
    state.model.pending_speed = None;
    if let Some(tb) = state.trackbar {
        unsafe {
            SendMessageW(
                tb,
                TBM_SETPOS,
                Some(WPARAM(1)),
                Some(LPARAM(state.model.speed_val as isize)),
            );
        }
    }
    sync_tip(state);
}

/// 滚轮速度落盘 + 模型 + 滑块 + tooltip 的单一入口。
fn wheel_set(state: &mut HostState, v: u32) {
    crate::speed::set_wheel(v);
    state.model.wheel_val = v.clamp(1, 100);
    state.model.pending_wheel = None;
    if let Some(tb) = state.wheel_trackbar {
        unsafe {
            SendMessageW(
                tb,
                TBM_SETPOS,
                Some(WPARAM(1)),
                Some(LPARAM(state.model.wheel_val as isize)),
            );
        }
    }
    sync_tip(state);
}

/// 菜单键盘 ←/→ 的离散调速。
fn nudge_speed(state: &mut HostState, delta: i32) {
    let v = (state.model.speed_val as i32 + delta).clamp(1, 20) as u32;
    if v != state.model.speed_val {
        speed_set(state, v);
    }
}

fn invalidate_state(state: &HostState) {
    if let Some(h) = state.menu {
        unsafe {
            let _ = InvalidateRect(Some(h), None, false.into());
        }
    }
}

// ── 绘制 ──────────────────────────────────────────────

fn paint(ptr: *mut HostState, hwnd: HWND) {
    unsafe {
        let state = &mut *ptr;
        let mut ps = PAINTSTRUCT::default();
        let hdc: HDC = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let w = rc.right.max(1);
        let h = rc.bottom.max(1);

        // 内存 DC 双缓冲
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old_bmp = SelectObject(mem, bmp.into());
        draw_menu(state, mem, dpi_scale(hwnd), w, h);
        let _ = windows::Win32::Graphics::Gdi::BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old_bmp);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn draw_menu(state: &HostState, hdc: HDC, s: f32, w: i32, h: i32) {
    unsafe {
        let model = &state.model;
        let n = model.ruled_devs.len();
        let n_other = model.other_devs.len();
        let pal = current_pal(state);

        // 背景 + 边框
        let full = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        let bg_brush = CreateSolidBrush(pal.bg);
        FillRect(hdc, &full, bg_brush);
        let _ = DeleteObject(bg_brush.into());
        let border_brush = CreateSolidBrush(pal.border);
        FrameRect(hdc, &full, border_brush);
        let _ = DeleteObject(border_brush.into());

        let old_font = SelectObject(hdc, ui_font(state).into());
        SetBkMode(hdc, TRANSPARENT);

        // ── 标题行：指针速度: N + 恢复默认按钮 ──
        // 拖动中显示预览值（灰色提示尚未应用），松手才真正改系统速度
        let title_text = format!("指针速度: {}", model.display_speed());
        let title_color = if model.pending_speed.is_some() {
            pal.gray
        } else {
            pal.text
        };
        draw_text(hdc, &title_text, PAD, TITLE_H / 2.0, s, title_color);

        let (rl, rt, rr, rb) = reset_btn_rect();
        let hovered_reset = state.hover == Some(Hover::Reset);
        let pressed_reset = state.pressed == Some(Hover::Reset);
        let btn = rect_px(rl + 0.5, rt + 0.5, rr - 0.5, rb - 0.5, s);
        if pressed_reset {
            FillRect(hdc, &btn, CreateSolidBrush(pal.btn_pressed));
        } else if hovered_reset {
            FillRect(hdc, &btn, CreateSolidBrush(pal.btn_hover));
        }
        let pen = CreatePen(PS_SOLID, 1, pal.border);
        let old_pen = SelectObject(hdc, pen.into());
        // Rectangle 默认会用 DC 当前画刷（白）填内部，把上面的悬停底色盖掉——
        // 套 NULL_BRUSH 让它只描边
        let old_brush = SelectObject(
            hdc,
            windows::Win32::Graphics::Gdi::GetStockObject(
                windows::Win32::Graphics::Gdi::NULL_BRUSH,
            )
            .into(),
        );
        let _ =
            windows::Win32::Graphics::Gdi::Rectangle(hdc, btn.left, btn.top, btn.right, btn.bottom);
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
        let _ = DeleteObject(pen.into());
        let btn_color = if pressed_reset || hovered_reset {
            pal.hl_text
        } else {
            pal.text
        };
        draw_text_center(hdc, "恢复默认", rl, rt, rr, rb, s, btn_color);

        // ── 滚轮标签行 + 滚轮滑块（真控件）──
        let wheel_text = format!("滚轮: {}", model.display_wheel());
        let wheel_color = if model.pending_wheel.is_some() {
            pal.gray
        } else {
            pal.text
        };
        draw_text(
            hdc,
            &wheel_text,
            PAD,
            wheel_label_top() + TITLE_H / 2.0,
            s,
            wheel_color,
        );
        sep(hdc, wheel_slider_top() + SLIDER_H, s, w, pal.sep);

        // ── 生效规则：三行（规则名 / 两个速度 / 滚轮模式）──
        let eff_y = eff_row_top();
        match &model.effective {
            Some(info) => {
                let EffectiveInfo {
                    name,
                    speed,
                    wheel,
                    scroll,
                } = info;
                let wheel_str = wheel.map_or("保持".to_string(), |w| w.to_string());
                let (scroll_on, scroll_trigger, scroll_kb, scroll_px) = scroll
                    .as_ref()
                    .map_or(
                        (false, TriggerBtn::X1, None, SCROLL_PX_DEFAULT),
                        |s| {
                            (
                                s.enabled,
                                s.trigger,
                                s.kb_trigger,
                                s.px_per_line,
                            )
                        },
                    );
                let kb_label = scroll_kb
                    .map_or("无".to_string(), |k| format!("键盘 {}", k.label()));
                let scroll_line = if scroll_on {
                    format!(
                        "滚轮模式: 开 · {} · {kb_label} · {scroll_px} 像素/行",
                        scroll_trigger.label()
                    )
                } else {
                    format!(
                        "滚轮模式: 关 · {} · {kb_label} · {scroll_px} 像素/行",
                        scroll_trigger.label()
                    )
                };
                draw_text(
                    hdc,
                    &format!("生效规则: {name}"),
                    PAD,
                    eff_y + INFO_ROW_H / 2.0,
                    s,
                    pal.gray,
                );
                draw_text(
                    hdc,
                    &format!("指针: {speed} · 滚轮: {wheel_str}"),
                    PAD,
                    eff_y + INFO_ROW_H * 1.5,
                    s,
                    pal.gray,
                );
                draw_text(
                    hdc,
                    &scroll_line,
                    PAD,
                    eff_y + INFO_ROW_H * 2.5,
                    s,
                    pal.gray,
                );
            }
            None => {
                draw_text(
                    hdc,
                    "生效规则: 无",
                    PAD,
                    eff_y + EFF_INFO_H / 2.0,
                    s,
                    pal.gray,
                );
            }
        }

        sep(hdc, eff_y + EFF_INFO_H, s, w, pal.sep);

        // ── 设备行（只显示有规则设备）──
        if n == 0 {
            let py = device_row_top(0) + INFO_ROW_H / 2.0;
            draw_text(
                hdc,
                "无生效规则设备",
                PAD,
                py,
                s,
                pal.gray,
            );
        }
        for (local, &global) in model.ruled_devs.iter().enumerate() {
            let d = &model.devs[global];
            let top = device_row_top(local);
            let hovered = state.hover == Some(Hover::Device(local));
            let focused = model.kb_focus == Some(local);
            let color = if hovered { pal.hl_text } else { pal.text };
            draw_row_bg(
                hdc,
                top,
                ROW_H,
                s,
                w,
                hovered,
                false,
                focused,
                pal.highlight,
                pal.row_pressed,
            );
            if d.is_effective && d.rule_speed.is_some() {
                draw_check(hdc, top + ROW_H / 2.0, s, color);
            }
            if d.rule_speed.is_some() {
                draw_gear(hdc, top + ROW_H / 2.0, s, color);
            }
            if d.rule_scroll.as_ref().map_or(false, |s| s.enabled) {
                draw_scroll(hdc, top + ROW_H / 2.0, s, color);
            }
            draw_text(
                hdc,
                &d.row_text(),
                PAD + CHECK_W
                    + RULE_ICON_W
                    + if d.rule_scroll.is_some() {
                        RULE_ICON_W
                    } else {
                        0.0
                    },
                top + ROW_H / 2.0,
                s,
                color,
            );
            // 悬停展开单设备配置子菜单：右缘画展开指示
            draw_sub_arrow(hdc, MENU_W - PAD, top + ROW_H / 2.0, s, color);
        }

        // ── 「其他设备」入口行 ──
        let other_top = other_row_top(n);
        let other_hovered = state.hover == Some(Hover::OtherDevices);
        let other_focused = model.kb_focus == Some(n);
        let other_color = if other_hovered || other_focused {
            pal.hl_text
        } else {
            pal.text
        };
        draw_row_bg(
            hdc,
            other_top,
            ROW_H,
            s,
            w,
            other_hovered,
            false,
            other_focused,
            pal.highlight,
            pal.row_pressed,
        );
        draw_text(
            hdc,
            &other_entry_text(n_other),
            PAD + CHECK_W,
            other_top + ROW_H / 2.0,
            s,
            other_color,
        );
        draw_sub_arrow(hdc, MENU_W - PAD, other_top + ROW_H / 2.0, s, other_color);

        let auto_top = autostart_row_top(n);
        sep(hdc, auto_top - SEP_H, s, w, pal.sep);

        // ── 开机自启 ──
        {
            let hovered = state.hover == Some(Hover::Autostart);
            let pressed = state.pressed == Some(Hover::Autostart);
            let focused = model.kb_focus == Some(n + 1);
            let color = if hovered || pressed {
                pal.hl_text
            } else {
                pal.text
            };
            draw_row_bg(
                hdc,
                auto_top,
                ROW_H,
                s,
                w,
                hovered,
                pressed,
                focused,
                pal.highlight,
                pal.row_pressed,
            );
            if model.autostart_on {
                draw_check(hdc, auto_top + ROW_H / 2.0, s, color);
            }
            draw_text(
                hdc,
                "开机自启",
                PAD + CHECK_W,
                auto_top + ROW_H / 2.0,
                s,
                color,
            );
        }

        let exit_top = exit_row_top(n);
        sep(hdc, exit_top - SEP_H, s, w, pal.sep);

        // ── 退出 ──
        {
            let hovered = state.hover == Some(Hover::Exit);
            let pressed = state.pressed == Some(Hover::Exit);
            let focused = model.kb_focus == Some(n + 2);
            let color = if hovered || pressed {
                pal.hl_text
            } else {
                pal.text
            };
            draw_row_bg(
                hdc,
                exit_top,
                ROW_H,
                s,
                w,
                hovered,
                pressed,
                focused,
                pal.highlight,
                pal.row_pressed,
            );
            draw_text(hdc, "退出", PAD + CHECK_W, exit_top + ROW_H / 2.0, s, color);
        }

        SelectObject(hdc, old_font);
    }
}

fn rect_px(l: f32, t: f32, r: f32, b: f32, s: f32) -> RECT {
    RECT {
        left: (l * s).round() as i32,
        top: (t * s).round() as i32,
        right: (r * s).round() as i32,
        bottom: (b * s).round() as i32,
    }
}

/// 行背景：悬停整行高亮；按压用更深一档的颜色；键盘焦点画系统焦点框。
pub(crate) fn draw_row_bg(
    hdc: HDC,
    top_dip: f32,
    h_dip: f32,
    s: f32,
    w: i32,
    hovered: bool,
    pressed: bool,
    focused: bool,
    highlight: COLORREF,
    row_pressed: COLORREF,
) {
    unsafe {
        let r = RECT {
            left: 0,
            top: (top_dip * s).round() as i32,
            right: w,
            bottom: ((top_dip + h_dip) * s).round() as i32,
        };
        if pressed {
            FillRect(hdc, &r, CreateSolidBrush(row_pressed));
        } else if hovered {
            FillRect(hdc, &r, CreateSolidBrush(highlight));
        }
        if focused && !hovered && !pressed {
            let _ = DrawFocusRect(hdc, &r);
        }
    }
}

fn sep(hdc: HDC, y_dip: f32, s: f32, w: i32, sep_color: COLORREF) {
    unsafe {
        let pen = CreatePen(PS_SOLID, 1, sep_color);
        let old = SelectObject(hdc, pen.into());
        let y = ((y_dip + SEP_H / 2.0) * s).round() as i32;
        let _ = MoveToEx(hdc, (PAD * s).round() as i32, y, None);
        let _ = LineTo(hdc, (((MENU_W - PAD) * s).round() as i32).min(w), y);
        SelectObject(hdc, old);
        let _ = DeleteObject(pen.into());
    }
}

/// 对勾（与 egui 版同形状：左中 → 下中 → 右上，2px）。
pub(crate) fn draw_check(hdc: HDC, cy_dip: f32, s: f32, color: COLORREF) {
    unsafe {
        let cx = (PAD + CHECK_W / 2.0) * s;
        let cy = cy_dip * s;
        let u = 4.0 * s;
        let pen = CreatePen(PS_SOLID, (2.0 * s).round().max(1.0) as i32, color);
        let old = SelectObject(hdc, pen.into());
        let _ = MoveToEx(hdc, (cx - u).round() as i32, cy.round() as i32, None);
        let _ = LineTo(hdc, (cx - u / 3.0).round() as i32, (cy + u).round() as i32);
        let _ = LineTo(hdc, (cx + u).round() as i32, (cy - u).round() as i32);
        SelectObject(hdc, old);
        let _ = DeleteObject(pen.into());
    }
}

/// 子菜单展开指示（右缘描边小箭头 ›）。`right_dip` 为箭头尖端的 x（DIP），
/// 调用方传 `MENU_W - PAD` 或子菜单窗口的 `宽 - PAD`。
pub(crate) fn draw_sub_arrow(hdc: HDC, right_dip: f32, cy_dip: f32, s: f32, color: COLORREF) {
    unsafe {
        let tip = right_dip * s;
        let cy = cy_dip * s;
        let u = 4.0 * s;
        let pen = CreatePen(PS_SOLID, (1.5 * s).round().max(1.0) as i32, color);
        let old = SelectObject(hdc, pen.into());
        let _ = MoveToEx(hdc, (tip - u).round() as i32, (cy - u).round() as i32, None);
        let _ = LineTo(hdc, tip.round() as i32, cy.round() as i32);
        let _ = LineTo(hdc, (tip - u).round() as i32, (cy + u).round() as i32);
        SelectObject(hdc, old);
        let _ = DeleteObject(pen.into());
    }
}

/// 小齿轮图标（表示该设备已配置规则），绘制在对勾右侧的规则图标列。
fn draw_gear(hdc: HDC, cy_dip: f32, s: f32, color: COLORREF) {
    unsafe {
        let cx = (PAD + CHECK_W + RULE_ICON_W / 2.0) * s;
        let cy = cy_dip * s;
        let r_inner = 3.5 * s;
        let r_outer = 5.5 * s;
        let n_teeth = 8;
        let two_pi = 2.0 * std::f32::consts::PI;
        let step = two_pi / n_teeth as f32;
        let half = step / 2.0;
        let offset = -std::f32::consts::PI / 2.0;

        let mut pts = [POINT { x: 0, y: 0 }; 16];
        for i in 0..n_teeth {
            let a_v = offset + i as f32 * step - half;
            pts[2 * i] = POINT {
                x: (cx + r_inner * a_v.cos()).round() as i32,
                y: (cy + r_inner * a_v.sin()).round() as i32,
            };
            let a_t = offset + i as f32 * step;
            pts[2 * i + 1] = POINT {
                x: (cx + r_outer * a_t.cos()).round() as i32,
                y: (cy + r_outer * a_t.sin()).round() as i32,
            };
        }

        let pen = CreatePen(PS_SOLID, (2.0 * s).round().max(1.0) as i32, color);
        let old = SelectObject(hdc, pen.into());
        let _ = MoveToEx(hdc, pts[0].x, pts[0].y, None);
        for i in 1..pts.len() {
            let _ = LineTo(hdc, pts[i].x, pts[i].y);
        }
        // 闭合轮廓
        let _ = LineTo(hdc, pts[0].x, pts[0].y);
        SelectObject(hdc, old);
        let _ = DeleteObject(pen.into());
    }
}

/// 滚轮模式图标（小鼠标轮廓 + 轮线），绘制在齿轮右侧的滚轮图标列。
/// 椭圆会按当前画刷填充，先选 NULL_BRUSH 只留轮廓（暗色主题下不能填白）。
fn draw_scroll(hdc: HDC, cy_dip: f32, s: f32, color: COLORREF) {
    unsafe {
        let cx = (PAD + CHECK_W + RULE_ICON_W * 1.5) * s;
        let cy = cy_dip * s;
        let rx = 4.0 * s;
        let ry = 5.5 * s;
        let pen = CreatePen(PS_SOLID, (1.5 * s).round().max(1.0) as i32, color);
        let old_pen = SelectObject(hdc, pen.into());
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let _ = Ellipse(
            hdc,
            (cx - rx).round() as i32,
            (cy - ry).round() as i32,
            (cx + rx).round() as i32,
            (cy + ry).round() as i32,
        );
        SelectObject(hdc, old_brush);
        // 滚轮：上半部一小段竖线
        let _ = MoveToEx(
            hdc,
            cx.round() as i32,
            (cy - 3.5 * s).round() as i32,
            None,
        );
        let _ = LineTo(hdc, cx.round() as i32, (cy - 1.0 * s).round() as i32);
        SelectObject(hdc, old_pen);
        let _ = DeleteObject(pen.into());
    }
}

/// 左对齐文本，垂直居中于 cy。
pub(crate) fn draw_text(hdc: HDC, text: &str, x_dip: f32, cy_dip: f32, s: f32, color: COLORREF) {
    unsafe {
        SetTextColor(hdc, color);
        let mut w16: Vec<u16> = text.encode_utf16().collect();
        let mut rc = RECT {
            left: (x_dip * s).round() as i32,
            top: ((cy_dip - 12.0) * s).round() as i32,
            right: (MENU_W * s).round() as i32,
            bottom: ((cy_dip + 12.0) * s).round() as i32,
        };
        let _ = DrawTextW(
            hdc,
            &mut w16,
            &mut rc,
            DT_SINGLELINE | DT_LEFT | DT_VCENTER | DT_NOPREFIX,
        );
    }
}

/// 矩形内居中文本（按钮用）。
pub(crate) fn draw_text_center(
    hdc: HDC,
    text: &str,
    l: f32,
    t: f32,
    r: f32,
    b: f32,
    s: f32,
    color: COLORREF,
) {
    unsafe {
        SetTextColor(hdc, color);
        let mut w16: Vec<u16> = text.encode_utf16().collect();
        let mut rc = rect_px(l, t, r, b, s);
        let _ = DrawTextW(
            hdc,
            &mut w16,
            &mut rc,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX,
        );
    }
}
