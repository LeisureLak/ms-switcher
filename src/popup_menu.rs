//! 自绘托盘菜单（内嵌滑动条）。
//!
//! Windows 原生 HMENU 无法嵌入交互控件，因此用 WS_POPUP 窗口整体替换
//! TrackPopupMenu：顶部为滑动条区（标签 + Trackbar + 恢复默认按钮，
//! 拖动实时调速），下方为自绘菜单项（设备列表、规则操作、自启、退出）。
//! 外观模仿原生菜单：系统菜单字体、COLOR_MENU 背景、COLOR_HIGHLIGHT 悬停。

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};


use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, TBM_SETRANGE, TBM_SETPOS,
    TBM_SETTICFREQ, TBS_AUTOTICKS, TRACKBAR_CLASSW, WM_MOUSELEAVE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::speed;

/// TBM_GETPOS = 0x0400（windows crate 0.62.2 未导出，按 SDK 值补上）。
const TBM_GETPOS: u32 = 1024;

// ── 布局（96 DPI 基准，创建时按 DPI 缩放）──────────────
/// 弹窗宽度基准。
const WIN_W: i32 = 320;
/// 标题区（标签 + 恢复默认按钮）高。
const HDR_H: i32 = 34;
/// Trackbar 区高。
const TBAR_H: i32 = 34;
/// 单条菜单项行高（会按字体度量调整，此为下限基准）。
const ROW_H: i32 = 26;
/// 分隔线行高。
const SEP_H: i32 = 9;
/// 左右内边距。
const PAD_X: i32 = 8;
/// 滑动条区与菜单项区之间的分隔行高。
const DIVIDER_H: i32 = 4;

// ── 子控件 ID ──────────────────────────────────────────
const CTRL_LABEL: i32 = 1;
const CTRL_TRACKBAR: i32 = 2;
const CTRL_RESET: i32 = 3;

// ── 菜单项模型（纯数据，可单测）─────────────────────────

/// 一条菜单项。命令 ID 沿用 main.rs 的 IDM_* 体系，点击后原样回传命令处理器。
#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    /// 设备标题行（不可点，仅展示）。
    DeviceTitle,
    /// 灰字信息行（VID:PID 等，不可点）。
    Info,
    /// 可点命令项。`enabled = false` 时置灰不可点。
    Action { cmd: u32, enabled: bool },
    /// 可点命令项，带勾选态（开机自启）。
    Checkable { cmd: u32, checked: bool },
    /// 分隔线。
    Separator,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    pub text: String,
    /// 布局产物：项顶部的 y 坐标（窗口客户区，物理像素）。
    pub y: i32,
    /// 布局产物：项高度。
    pub h: i32,
    /// DeviceTitle 行所属的设备索引（其余行为 0）。
    pub device_idx: usize,
    /// 是否画勾选标记（生效中的规则设备行 / 开机自启）。
    pub checked: bool,
}

impl Item {
    fn new(kind: ItemKind, text: impl Into<String>) -> Item {
        Item {
            kind,
            text: text.into(),
            y: 0,
            h: 0,
            device_idx: 0,
            checked: false,
        }
    }

    /// 是否可交互（悬停高亮 + 命中测试包含此行）。
    /// 设备标题行可悬停（触发展开），但点击不产生命令。
    pub fn clickable(&self) -> bool {
        match &self.kind {
            ItemKind::DeviceTitle => true,
            ItemKind::Action { enabled, .. } => *enabled,
            ItemKind::Checkable { .. } => true,
            _ => false,
        }
    }

    /// 关联的命令 ID（可执行项才有；设备标题行悬停仅触发展开，无命令）。
    pub fn cmd(&self) -> Option<u32> {
        match &self.kind {
            ItemKind::Action { cmd, .. } | ItemKind::Checkable { cmd, .. } => Some(*cmd),
            _ => None,
        }
    }

    /// 设备索引（DeviceTitle 行才有）。
    pub fn device_index(&self) -> Option<usize> {
        match &self.kind {
            ItemKind::DeviceTitle => Some(self.device_idx),
            _ => None,
        }
    }
}

/// 一台设备的菜单数据（从 devices::MouseDevice 提炼，纯数据便于单测）。
pub struct DevMenu {
    pub name: String,
    pub vid: Option<String>,
    pub pid: Option<String>,
    /// 当前规则速度（None = 无规则）。
    pub rule_speed: Option<u32>,
    /// 该设备的规则是否为当前生效规则（最后插入的规则设备）。
    pub is_effective: bool,
    /// 三个设备子项的命令 ID（保存规则 / 删除规则 / 重新应用）。
    pub cmds: [u32; 3],
}

/// 菜单顶部说明行（多规则并存时标明当前生效者）。
pub struct MenuHeader {
    /// 仅当生效规则设备数 >= 2 时提供：「生效规则: name (速度 sp)」。
    pub effective_text: Option<String>,
    /// 开机自启当前状态（勾选标记）。
    pub autostart_on: bool,
}

/// 组装菜单项列表（不含滑动条区；布局坐标由 layout_items 填充）。
///
/// `expanded_device`：当前悬停展开子菜单的设备索引（None = 全部收起）。
/// 行为与原生菜单二级子项一致：设备标题行悬停时展开其子项。
///
/// 恢复旧原生菜单的展示逻辑：
/// - 设备行文本带规则标记（`· 规则 N`），生效中的设备加 `✓生效中` 文本并画勾选标记；
/// - 两个以上规则设备并存时，顶部加一行灰字说明当前生效者。
pub fn build_items(
    devs: &[DevMenu],
    expanded_device: Option<usize>,
    header: &MenuHeader,
) -> Vec<Item> {
    let mut items = Vec::new();

    // 多规则并存：顶部一行说明当前生效者（沿用旧菜单行为）
    if let Some(t) = &header.effective_text {
        items.push(Item::new(ItemKind::Info, t.clone()));
        items.push(Item::new(ItemKind::Separator, ""));
    }

    if devs.is_empty() {
        items.push(Item::new(ItemKind::Info, "未检测到鼠标设备"));
        items.push(Item::new(ItemKind::Separator, ""));
    }
    for (i, d) in devs.iter().enumerate() {
        let vid = d.vid.as_deref().unwrap_or("----");
        let pid = d.pid.as_deref().unwrap_or("----");
        // 设备行带规则标记（旧菜单格式）：`name [vid:pid] · 规则 N ✓生效中`
        let mut text = format!("{}  [{}:{}]", d.name, vid, pid);
        if let Some(sp) = d.rule_speed {
            text.push_str(&format!("  · 规则 {}", sp));
            if d.is_effective {
                text.push_str("  ✓生效中");
            }
        }
        let mut title = Item::new(ItemKind::DeviceTitle, text);
        title.device_idx = i;
        title.checked = d.is_effective && d.rule_speed.is_some();
        items.push(title);
        // 仅悬停中的设备展开子项（同原生二级菜单行为）
        if expanded_device == Some(i) {
            // 设备详情信息行（旧菜单的 VID/PID 展示，灰字）
            items.push(Item::new(ItemKind::Info, format!("VID:{}  PID:{}", vid, pid)));
            let can_rule = d.vid.is_some() && d.pid.is_some();
            let [c_set, c_del, c_reapply] = d.cmds;
            items.push(Item::new(
                ItemKind::Action { cmd: c_set, enabled: can_rule },
                "用当前速度保存规则",
            ));
            items.push(Item::new(
                ItemKind::Action { cmd: c_del, enabled: can_rule && d.rule_speed.is_some() },
                "删除此设备规则",
            ));
            items.push(Item::new(
                ItemKind::Action { cmd: c_reapply, enabled: can_rule },
                "重新应用规则",
            ));
        }
        items.push(Item::new(ItemKind::Separator, ""));
    }
    items.push(Item::new(
        ItemKind::Checkable { cmd: IDM_AUTOSTART, checked: header.autostart_on },
        "开机自启",
    ));
    items
}

/// 「开机自启」命令 ID（与 main.rs IDM_AUTOSTART 一致）。
const IDM_AUTOSTART: u32 = 901;

/// 填充布局：从 `y0` 开始逐项排布（物理像素）。返回总底部 y。
pub fn layout_items(items: &mut [Item], y0: i32, row_h: i32, sep_h: i32) -> i32 {
    let mut y = y0;
    for it in items.iter_mut() {
        it.y = y;
        it.h = match it.kind {
            ItemKind::Separator => sep_h,
            _ => row_h,
        };
        y += it.h;
    }
    y
}

/// 命中测试：返回 (索引, 是否可点)。不可点/未命中返回 None。
pub fn hit_test(items: &[Item], y: i32) -> Option<usize> {
    items
        .iter()
        .position(|it| y >= it.y && y < it.y + it.h && it.clickable())
}

// ── 窗口 ───────────────────────────────────────────────

/// 弹窗句柄的 Send 包装（仅主线程访问，与 tray.rs 同理）。
struct MenuWnd(HWND);
unsafe impl Send for MenuWnd {}

static OPEN: Mutex<Option<MenuWnd>> = Mutex::new(None);
static CLASS_ONCE: Once = Once::new();
static CLASS_OK: AtomicBool = AtomicBool::new(false);

const MENU_CLASS: PCWSTR = w!("MouseSpeedSwitcherPopupMenu");

/// 弹出菜单；已打开则置前。`on_command` 在菜单项被点击时回传命令 ID。
///
/// 设备子项悬停展开：`devs`/`header` 保存在 MenuState，
/// 悬停设备标题行时动态重建 items 并调整窗口高度（同原生二级菜单行为）。
pub fn show(parent: HWND, devs: Vec<DevMenu>, header: MenuHeader, on_command: fn(u32)) {
    let hwnd = {
        let mut guard = OPEN.lock().unwrap();
        if let Some(MenuWnd(existing)) = &*guard {
            unsafe {
                let _ = SetForegroundWindow(*existing);
            }
            return;
        }
        let hinst: HINSTANCE = match unsafe { GetModuleHandleW(None) } {
            Ok(h) => h.into(),
            Err(_) => {
                return;
            }
        };
        ensure_class(hinst);
        if !CLASS_OK.load(Ordering::Relaxed) {
            return;
        }
        unsafe {
            let mut icc: INITCOMMONCONTROLSEX = zeroed();
            icc.dwSize = size_of::<INITCOMMONCONTROLSEX>() as u32;
            icc.dwICC = ICC_BAR_CLASSES;
            let _ = InitCommonControlsEx(&icc);
        }

        let dpi = unsafe { GetDpiForWindow(parent) };
        let s = |v: i32| (v * dpi as i32 + 48) / 96;
        let w = s(WIN_W);
        let slider_zone = s(HDR_H + TBAR_H + DIVIDER_H);
        let row_h = s(ROW_H).max(s(20));
        let sep_h = s(SEP_H);
        // 初始全部收起（同原生菜单：悬停才展开）
        let mut items = build_items(&devs, None, &header);
        let bottom = layout_items(&mut items, slider_zone, row_h, sep_h);
        let h = bottom + s(4);

        // 光标附近弹出，夹取到工作区
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let mut x = pt.x;
        let mut y = pt.y;
        let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        let mut mi: MONITORINFO = unsafe { zeroed() };
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
            x = x.clamp(mi.rcWork.left, mi.rcWork.right - w);
            y = y.clamp(mi.rcWork.top, mi.rcWork.bottom - h);
        }

        // 每实例状态经堆分配传入（Box<MenuState>，WM_CREATE 接管）
        let state = Box::into_raw(Box::new(MenuState {
            items,
            devs,
            header,
            slider_zone,
            row_h,
            sep_h,
            expanded_device: None,
            hover: None,
            menu_font: HFONT::default(),
            created_ms: now_ms(),
            on_command,
            speed_label: String::new(),
        }));

        let hwnd = match unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                MENU_CLASS,
                w!("MouseSpeedSwitcherMenu"),
                WS_POPUP | WS_BORDER | WS_VISIBLE | WS_CLIPCHILDREN,
                x,
                y,
                w,
                h,
                Some(parent),
                None,
                Some(hinst),
                Some(state.cast()),
            )
        } {
            Ok(h) => {
                h
            }
            Err(e) => {
                unsafe {
                    drop(Box::from_raw(state));
                }
                return;
            }
        };
        *guard = Some(MenuWnd(hwnd));
        hwnd
    };

    unsafe {
        let _ = SetForegroundWindow(hwnd);
        // 实验：不给 Trackbar 焦点，排查重绘风暴
        // if let Ok(tb) = GetDlgItem(Some(hwnd), CTRL_TRACKBAR) {
        //     let _ = SetFocus(Some(tb));
        // }
    }
}

struct MenuState {
    items: Vec<Item>,
    /// 设备数据（悬停展开时重建 items 用）。
    devs: Vec<DevMenu>,
    /// 顶部说明（多规则生效者）。
    header: MenuHeader,
    /// 滑动条区总高（布局起点，物理像素）。
    slider_zone: i32,
    row_h: i32,
    sep_h: i32,
    /// 当前悬停展开的设备索引。
    expanded_device: Option<usize>,
    hover: Option<usize>,
    menu_font: HFONT,
    /// 创建时刻（ms），失活宽限期判断用。
    created_ms: u64,
    /// 命令回调：点击菜单项时回传命令 ID。
    on_command: fn(u32),
    /// 滑动条区标题文本（paint 自绘）。
    speed_label: String,
}

/// 点击后需要关闭菜单的命令（退出、设备规则操作等；重新加载配置可保持打开）。
const MENU_CLOSE_CMDS: &[u32] = &[902, 100, 101, 102];

fn ensure_class(hinst: HINSTANCE) {
    CLASS_ONCE.call_once(|| {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(menu_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: HICON::default(),
            hCursor: HCURSOR::default(),
            hbrBackground: unsafe { GetSysColorBrush(COLOR_MENU) },
            lpszMenuName: PCWSTR::null(),
            lpszClassName: MENU_CLASS,
        };
        let ok = unsafe { RegisterClassW(&wc) } != 0;
        CLASS_OK.store(ok, Ordering::Relaxed);
    });
}

unsafe extern "system" fn menu_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT { unsafe {
    match msg {
        // 必须显式转交 DefWindowProc 并返回其结果；否则 WM_NCCREATE 路径异常导致创建中止
        WM_NCCREATE => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        WM_CREATE => {
            let create: &CREATESTRUCTW = &*(lparam.0 as *const CREATESTRUCTW);
            let mut state = Box::from_raw(create.lpCreateParams as *mut MenuState);
            let dpi = unsafe { GetDpiForWindow(hwnd) };
            let s = |v: i32| (v * dpi as i32 + 48) / 96;

            // 系统菜单字体（lfMenuFont）
            let mut ncm: NONCLIENTMETRICSW = zeroed();
            ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
            let _ = SystemParametersInfoW(
                SPI_GETNONCLIENTMETRICS,
                ncm.cbSize,
                Some(&mut ncm as *mut _ as *mut c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
            state.menu_font = unsafe { CreateFontIndirectW(&ncm.lfMenuFont) };

            let state = Box::into_raw(state);
            unsafe {
                let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            }

            // ── 滑动条区子控件 ──
            // 标签不用 STATIC 子控件：STATIC 会被父窗口擦背景反复覆盖引发重绘风暴，
            // 改由 paint() 自绘文字。
            let tb_style =
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | TBS_AUTOTICKS);
            let trackbar = create_child(
                hwnd,
                TRACKBAR_CLASSW,
                CTRL_TRACKBAR,
                tb_style,
                s(PAD_X),
                s(HDR_H),
                s(WIN_W - PAD_X * 2),
                s(TBAR_H - 8),
            );
            let reset = create_child(
                hwnd,
                w!("BUTTON"),
                CTRL_RESET,
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                s(WIN_W - PAD_X - 100),
                s(4),
                s(100),
                s(22),
            );
            set_text(reset, "恢复默认");

            for ctl in [trackbar, reset] {
                unsafe {
                    let _ = SendMessageW(
                        ctl,
                        WM_SETFONT,
                        Some(WPARAM((*state).menu_font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }

            let cur = speed::get();
            unsafe {
                let _ = SendMessageW(trackbar, TBM_SETRANGE, Some(WPARAM(1)), Some(makelparam(1, 20)));
                let _ = SendMessageW(trackbar, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(cur as isize)));
                let _ = SendMessageW(trackbar, TBM_SETTICFREQ, Some(WPARAM(2)), Some(LPARAM(0)));
            }
            let _ = update_label_title(hwnd, cur);
            LRESULT(0)
        }
        // 背景 由 WM_PAINT 的 paint() 统一擦除；这里返回 1 表示无需系统再擦。
        // （WS_CLIPCHILDREN 窗口的 BeginPaint DC 已裁剪子控件区域，不会覆盖它们）
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => unsafe { paint(hwnd) },
        WM_HSCROLL => {
            if lparam.0 != 0 {
                let tb = HWND(lparam.0 as *mut c_void);
                let pos = unsafe { SendMessageW(tb, TBM_GETPOS, None, None) }.0 as u32;
                apply_speed(hwnd, pos);
            }
            LRESULT(0)
        }
        WM_COMMAND => match loword(wparam) as i32 {
            CTRL_RESET => {
                if let Ok(tb) = unsafe { GetDlgItem(Some(hwnd), CTRL_TRACKBAR) } {
                    unsafe {
                        let _ = SendMessageW(tb, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(10)));
                    }
                    apply_speed(hwnd, 10);
                }
                LRESULT(0)
            }
            _ => LRESULT(0),
        },
        WM_MOUSEMOVE => {
            let y = (lparam.0 >> 16) as i16 as i32;
            on_mouse_move(hwnd, y)
        }
        WM_MOUSELEAVE => {
            set_hover(hwnd, None);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let y = (lparam.0 >> 16) as i16 as i32;
            on_click(hwnd, y)
        }
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                let r = unsafe { DestroyWindow(hwnd) };
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if loword(wparam) == WA_INACTIVE {
                // 失焦销毁。宽限期沿用 slider.rs 已验证的 500ms 模式：
                // 窗口刚弹出时 SetForegroundWindow 可能被系统短暂收回，不销毁。
                let state = state_ptr(hwnd);
                if !state.is_null() {
                    let created = (*state).created_ms;
                    if now_ms().saturating_sub(created) > ACTIVATE_GRACE_MS {
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if ptr != 0 {
                    let state = Box::from_raw(ptr as *mut MenuState);
                    if !state.menu_font.is_invalid() {
                        let _ = DeleteObject(state.menu_font.into());
                    }
                }
            }
            *OPEN.lock().unwrap() = None;
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}}

// ── 绘制与交互实现 ──────────────────────────────────────

/// 失活宽限期（ms）：窗口刚弹出时若激活被系统短暂收回，不销毁。
const ACTIVATE_GRACE_MS: u64 = 500;

/// 进程启动时刻基准：created_ms 与 now_ms 同基准，差值即窗口年龄。
static BOOT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn now_ms() -> u64 {
    BOOT.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// GWLP_USERDATA 里存的 MenuState 裸指针。
unsafe fn state_ptr(hwnd: HWND) -> *mut MenuState { unsafe {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if ptr == 0 {
        std::ptr::null_mut()
    } else {
        ptr as *mut MenuState
    }
}}

unsafe fn paint(hwnd: HWND) -> LRESULT { unsafe {
    let state = state_ptr(hwnd);
    if state.is_null() {
        return LRESULT(0);
    }
    let st = &mut *state;
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc.is_invalid() {
        return LRESULT(0);
    }

    let old_font = {
        let f = st.menu_font;
        if !f.is_invalid() {
            SelectObject(hdc, f.into())
        } else {
            HGDIOBJ::default()
        }
    };
    SetBkMode(hdc, TRANSPARENT);

    // 菜单项区起点 = 第一个 item 的 y（滑动条区在其上方）
    let rc = ps.rcPaint;
    let bg = GetSysColorBrush(COLOR_MENU);
    let _ = FillRect(hdc, &rc, bg);

    // DPI 缩放常量（窗口创建后固定，paint 时直接用）
    let dpi = GetDpiForWindow(hwnd);
    let pad = (PAD_X * dpi as i32 + 48) / 96;
    let indent = (14 * dpi as i32 + 48) / 96;
    let check_w = (16 * dpi as i32 + 48) / 96;

    // 滑动条区标题（自绘，替代原 STATIC 子控件）
    if !st.speed_label.is_empty() {
        let mut trc = rc;
        trc.left = pad;
        trc.top = s_v(4, dpi);
        trc.bottom = trc.top + s_v(16, dpi);
        trc.right = rc.right - s_v(110, dpi);
        SetTextColor(hdc, COLORREF(GetSysColor(COLOR_MENUTEXT)));
        let mut ws: Vec<u16> =
            st.speed_label.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = DrawTextW(
            hdc,
            &mut ws,
            &mut trc,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
    }

    for (i, it) in st.items.iter().enumerate() {
        let mut irc = rc;
        irc.left = 0;
        irc.right = rc.right;
        irc.top = it.y;
        irc.bottom = it.y + it.h;

        match it.kind {
            ItemKind::Separator => {
                // 上下留白 + 中间一条阴影线
                let mid = it.y + it.h / 2;
                let pen = CreatePen(PS_SOLID, 1, COLORREF(GetSysColor(COLOR_3DSHADOW)));
                let old_pen = SelectObject(hdc, pen.into());
                let _ = MoveToEx(hdc, irc.left + pad, mid, None);
                let _ = LineTo(hdc, irc.right - pad, mid);
                SelectObject(hdc, old_pen);
                let _ = DeleteObject(pen.into());
            }
            _ => {
                let hovered = st.hover == Some(i);
                if hovered && it.clickable() {
                    let hl = CreateSolidBrush(COLORREF(GetSysColor(COLOR_HIGHLIGHT)));
                    let _ = FillRect(hdc, &irc, hl);
                    let _ = DeleteObject(hl.into());
                    SetTextColor(hdc, COLORREF(GetSysColor(COLOR_HIGHLIGHTTEXT)));
                } else if !it.clickable() {
                    SetTextColor(hdc, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
                } else {
                    SetTextColor(hdc, COLORREF(GetSysColor(COLOR_MENUTEXT)));
                }

                // 文本（左对齐，垂直居中；缩进：子项比标题深一级）
                let mut trc = irc;
                trc.left = pad
                    + match it.kind {
                        ItemKind::DeviceTitle => 0,
                        _ => indent,
                    };
                trc.right -= pad + check_w;

                // 勾选标记（开机自启 / 生效中的规则设备行）
                if it.checked {
                    draw_check(hdc, irc, check_w, pad);
                }

                let mut ws: Vec<u16> = it.text.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = DrawTextW(
                    hdc,
                    &mut ws,
                    &mut trc,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                );
            }
        }
    }

    SelectObject(hdc, old_font);
    let _ = EndPaint(hwnd, &ps);
    LRESULT(0)
}}

/// 绘制对勾（勾选态菜单项，靠右）。
unsafe fn draw_check(hdc: HDC, irc: RECT, check_w: i32, pad: i32) { unsafe {
    let cx = irc.right - check_w / 2 - pad / 2;
    let cy = (irc.top + irc.bottom) / 2;
    let u = check_w / 4;
    let pen = CreatePen(PS_SOLID, 2, COLORREF(GetSysColor(COLOR_MENUTEXT)));
    let old = SelectObject(hdc, pen.into());
    let _ = MoveToEx(hdc, cx - u, cy, None);
    let _ = LineTo(hdc, cx - u / 3, cy + u);
    let _ = LineTo(hdc, cx + u, cy - u);
    SelectObject(hdc, old);
    let _ = DeleteObject(pen.into());
}}

fn on_mouse_move(hwnd: HWND, y: i32) -> LRESULT {
    unsafe {
        let state = state_ptr(hwnd);
        if state.is_null() {
            return LRESULT(0);
        }
        let st = &mut *state;
        let new_hover = hit_test(&st.items, y);
        if new_hover != st.hover {
            // 首次进入：注册 WM_MOUSELEAVE
            if st.hover.is_none() && new_hover.is_some() {
                let mut tme: TRACKMOUSEEVENT = zeroed();
                tme.cbSize = size_of::<TRACKMOUSEEVENT>() as u32;
                tme.dwFlags = TME_LEAVE;
                tme.hwndTrack = hwnd;
                let _ = TrackMouseEvent(&mut tme);
            }
            let old = st.hover;
            st.hover = new_hover;
            // 重绘受影响的两行
            for i in [old, new_hover].into_iter().flatten() {
                if let Some(it) = st.items.get(i) {
                    let r = RECT {
                        left: 0,
                        top: it.y,
                        right: 0x7FFF,
                        bottom: it.y + it.h,
                    };
                    let _ = InvalidateRect(Some(hwnd), Some(&r), true);
                }
            }
        }
        // 悬停设备标题行 → 展开该设备子项（同原生二级菜单行为）
        let hover_dev = new_hover
            .and_then(|i| st.items.get(i))
            .filter(|it| matches!(it.kind, ItemKind::DeviceTitle))
            .map(|it| it.device_idx);
        if hover_dev != st.expanded_device {
            expand_device(hwnd, state, hover_dev);
        }
    }
    LRESULT(0)
}

/// 切换悬停展开的设备：重建 items、重排布局、调整窗口高度并整体重绘。
fn expand_device(hwnd: HWND, state: *mut MenuState, dev: Option<usize>) {
    unsafe {
        let st = &mut *state;
        let mut items = build_items(&st.devs, dev, &st.header);
        let bottom = layout_items(&mut items, st.slider_zone, st.row_h, st.sep_h);
        let dpi = GetDpiForWindow(hwnd);
        let pad_bottom = (4 * dpi as i32 + 48) / 96;
        let new_h = bottom + pad_bottom;
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        let old_h = rc.bottom - rc.top;
        st.items = items;
        st.expanded_device = dev;
        st.hover = None; // 布局变了，旧索引作废；下一次 mousemove 会重新命中
        if new_h != old_h {
            // 顶部固定，只改高度（向下展开/收起）
            let _ = SetWindowPos(
                hwnd,
                None,
                rc.left,
                rc.top,
                rc.right - rc.left,
                new_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// 点击菜单项：回传命令给回调并按需关闭窗口。
/// 设备标题行点击只展开（无命令）；空命令可点项（如置灰行）无动作。
fn on_click(hwnd: HWND, y: i32) -> LRESULT {
    unsafe {
        let state = state_ptr(hwnd);
        if state.is_null() {
            return LRESULT(0);
        }
        let idx = hit_test(&(*state).items, y);
        if let Some(i) = idx {
            let (cmd, on_command) = {
                let st = &*state;
                (st.items[i].cmd(), st.on_command)
            };
            if let Some(cmd) = cmd {
                // 命令回调（由 main.rs 注入）
                let close = MENU_CLOSE_CMDS.contains(&cmd);
                on_command(cmd);
                if close {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }
    LRESULT(0)
}

fn set_hover(hwnd: HWND, idx: Option<usize>) {
    unsafe {
        let state = state_ptr(hwnd);
        if state.is_null() {
            return;
        }
        (*state).hover = idx;
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn apply_speed(hwnd: HWND, pos: u32) {
    speed::set(pos.clamp(1, 20));
    update_label_title(hwnd, pos.clamp(1, 20));
}

/// 更新标题文本（存入 MenuState，由 paint() 自绘）。
fn update_label_title(hwnd: HWND, pos: u32) {
    unsafe {
        let state = state_ptr(hwnd);
        if state.is_null() {
            return;
        }
        (*state).speed_label = format!("指针速度: {}", pos);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// paint 内的 DPI 缩放（96 基准）。
fn s_v(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

/// paint 内的 DPI 缩放（96 基准）。
fn s_v_old(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

fn create_child(
    parent: HWND,
    class: windows::core::PCWSTR,
    id: i32,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> HWND {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR::null(),
            style,
            x,
            y,
            w,
            h,
            Some(parent),
            Some(HMENU(id as isize as *mut c_void)),
            None,
            None,
        )
    }
    .unwrap_or_default()
}

fn set_text(hwnd: HWND, text: &str) {
    let ws: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(ws.as_ptr()));
    }
}

/// MAKELPARAM(l, h)。
fn makelparam(low: u16, high: u16) -> LPARAM {
    LPARAM(((high as isize) << 16) | low as isize)
}

fn loword(v: WPARAM) -> u32 {
    (v.0 & 0xFFFF) as u32
}

// ── 单元测试 ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, vid: Option<&str>, pid: Option<&str>, rule: Option<u32>) -> DevMenu {
        DevMenu {
            name: name.into(),
            vid: vid.map(Into::into),
            pid: pid.map(Into::into),
            rule_speed: rule,
            is_effective: false,
            cmds: [100, 101, 102],
        }
    }

    fn header() -> MenuHeader {
        MenuHeader { effective_text: None, autostart_on: false }
    }

    fn empty_header() -> MenuHeader {
        MenuHeader { effective_text: None, autostart_on: false }
    }

    #[test]
    fn collapsed_no_subitems() {
        let items = build_items(&[dev("轨迹球", Some("046d"), Some("c52b"), Some(4))], None, &empty_header());
        // 收起时：标题 + 分隔 + 自启 = 3
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].kind, ItemKind::DeviceTitle);
        assert!(items[0].clickable(), "设备标题可悬停触发展开");
        assert_eq!(items[0].cmd(), None, "设备标题点击无命令");
        assert_eq!(items[0].device_idx, 0);
    }

    #[test]
    fn expanded_shows_subitems() {
        let items = build_items(&[dev("轨迹球", Some("046d"), Some("c52b"), Some(4))], Some(0), &empty_header());
        // 标题 + Info(VID/PID) + 3 子项 + 分隔 + 自启 = 7
        assert_eq!(items.len(), 7);
        assert_eq!(items[0].kind, ItemKind::DeviceTitle);
        assert_eq!(items[1].kind, ItemKind::Info, "展开区第一行是 VID/PID 信息");
        assert_eq!(items[2].text, "用当前速度保存规则");
        let del = &items[3];
        assert!(del.clickable(), "有规则时删除规则可点");
    }

    #[test]
    fn device_row_carries_rule_marker() {
        let mut d = dev("ELECOM 轨迹球", Some("046d"), Some("c52b"), Some(4));
        d.is_effective = true;
        let items = build_items(&[d], None, &empty_header());
        let title = &items[0];
        assert!(title.text.contains("· 规则 4"), "设备行带规则标记: {}", title.text);
        assert!(title.text.contains("✓生效中"), "生效中带勾选文本: {}", title.text);
        assert!(title.checked, "生效设备行画勾选标记");
    }

    #[test]
    fn delete_rule_disabled_without_rule() {
        let items = build_items(&[dev("鼠标", Some("1"), Some("2"), None)], Some(0), &empty_header());
        // 标题 + Info + 3子项 + 分隔 + 自启
        assert!(!items[3].clickable(), "无规则时删除规则置灰");
        assert!(items[2].clickable());
        assert!(items[4].clickable());
    }

    #[test]
    fn all_disabled_without_vid_pid() {
        let items = build_items(&[dev("盲设备", None, None, None)], Some(0), &empty_header());
        assert!(!items[2].clickable());
        assert!(!items[3].clickable());
        assert!(!items[4].clickable());
        // VID/PID 信息行显示 ----
        assert!(items[1].text.contains("----"));
    }

    #[test]
    fn empty_devices_placeholder() {
        let items = build_items(&[], None, &empty_header());
        // 占位 + 分隔 + 自启
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].kind, ItemKind::Info);
        assert!(!items[0].clickable());
    }

    #[test]
    fn header_effective_line_when_multiple_rules() {
        let h = MenuHeader {
            effective_text: Some("生效规则: a (速度 4)".into()),
            autostart_on: false,
        };
        let items = build_items(&[dev("a", Some("v"), Some("p"), Some(4)), dev("b", Some("v"), Some("p"), Some(7))], None, &h);
        // 生效说明行 + 分隔 + 2*(标题+分隔) + 自启 = 7
        assert_eq!(items[0].kind, ItemKind::Info);
        assert!(items[0].text.contains("生效规则: a"));
        assert_eq!(items[1].kind, ItemKind::Separator);
    }

    #[test]
    fn no_header_line_for_single_rule() {
        let h = MenuHeader { effective_text: None, autostart_on: false };
        let items = build_items(&[dev("a", Some("v"), Some("p"), Some(4))], None, &h);
        assert_eq!(items[0].kind, ItemKind::DeviceTitle, "单规则时无顶部说明行");
    }

    #[test]
    fn layout_monotonic_and_totals() {
        let mut items = build_items(&[dev("a", Some("v"), Some("p"), Some(4)), dev("b", Some("v"), Some("p"), None)], None, &MenuHeader { effective_text: None, autostart_on: true });
        let bottom = layout_items(&mut items, 100, 26, 9);
        let rows = items.iter().filter(|i| i.kind != ItemKind::Separator).count() as i32;
        let seps = (items.len() as i32) - rows;
        assert_eq!(bottom, 100 + rows * 26 + seps * 9);
        let mut prev = 100;
        for it in &items {
            assert_eq!(it.y, prev);
            prev += it.h;
        }
    }

    #[test]
    fn hit_test_skips_disabled() {
        let mut items = build_items(&[dev("鼠标", Some("1"), Some("2"), None)], Some(0), &empty_header());
        layout_items(&mut items, 0, 26, 9);
        // items[3]（删除规则）置灰：命中它的 y 应返回 None
        let it = &items[3];
        assert_eq!(hit_test(&items, it.y + 1), None);
        // items[2]（保存规则）可点
        let it = &items[2];
        assert_eq!(hit_test(&items, it.y + 1), Some(2));
        // 设备标题行可命中（悬停展开）
        let it = &items[0];
        assert_eq!(hit_test(&items, it.y + 1), Some(0));
    }

    #[test]
    fn autostart_checked() {
        let h = MenuHeader { effective_text: None, autostart_on: true };
        let items = build_items(&[dev("a", Some("v"), Some("p"), None)], None, &h);
        let last = items.last().unwrap();
        assert_eq!(last.kind, ItemKind::Checkable { cmd: IDM_AUTOSTART, checked: true });
        assert!(last.text == "开机自启");
    }
}


