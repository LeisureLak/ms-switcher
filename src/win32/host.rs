//! 原生宿主：隐藏顶层窗口 + 阻塞消息循环 + GWLP_USERDATA 借用式状态。
//!
//! 所有权模型（规避踩坑记录 三-2 的双重释放）：
//! - [`HostState`] 由调用方 `Box` 唯一持有；
//! - `CreateWindowExW` 的 `lpParam` 只传 `&mut HostState` 借用指针；
//! - `WM_NCCREATE` 只把指针写入 `GWLP_USERDATA`，窗口过程永不 `Box::from_raw`；
//! - Box 在消息循环结束后由调用方释放（恰好一次）。
//!
//! 规避踩坑记录 三-3 的失活自毁：菜单失活只 `PostMessageW(WM_APP_CLOSE_MENU)`，
//! 从不同步 `DestroyWindow`；关闭时先从宿主状态摘除再销毁。
//!
//! 规避踩坑记录 三-1 的锁重入：不用任何全局 `Mutex` 保护窗口状态；跨窗口
//! 过程访问的少量标志（如菜单激活标记）用原子量，其余状态只经消息循环
//! 串行访问。
//!
//! 本模块 FFI 密集：unsafe fn 内部不再重复 unsafe 块。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::Graphics::Gdi::{DeleteObject, InvalidateRect};
use windows::Win32::UI::Controls::TBM_SETPOS;
use windows::Win32::UI::Shell::{NIM_SETFOCUS, Shell_NotifyIconW, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging as win;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetAncestor, GetClassNameW, GetMessageW, GetWindowLongPtrW, KillTimer, LoadCursorW,
    MSLLHOOKSTRUCT, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
    SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WindowFromPoint, CREATESTRUCTW, GA_ROOT, HHOOK, IDC_ARROW, MSG,
    WINDOW_EX_STYLE, WH_MOUSE_LL, WM_CLOSE, WM_DESTROY, WM_LBUTTONDOWN, WM_NCCREATE,
    WM_RBUTTONDOWN, WNDCLASSEXW,
};

use super::device_notify::MouseDevNotify;
use super::menu;
use super::submenu;
use super::tray::{self, Tray};
use crate::menu_model::{self, Hover, MenuModel};
use crate::{devices, speed, state::AppState};

/// 关闭菜单请求（应用私有消息；失活/Esc/点击外部都投递它）。
pub const WM_APP_CLOSE_MENU: u32 = win::WM_USER + 2;
/// 关闭设备子菜单请求（异步，处理时重新检查悬停状态）。
pub const WM_APP_CLOSE_SUBMENU: u32 = win::WM_USER + 3;
/// 子菜单滑块（子控件）的 leave 通知：光标可能从滑块直接移出子菜单窗口。
pub const WM_APP_SUB_LEFT: u32 = win::WM_USER + 4;

const HOST_CLASS: windows::core::PCWSTR = w!("MSS_Host");

/// 设备事件防抖：一次性 timer ID 与时长（设备插拔会连发多条消息）。
const TIMER_DEV: usize = 1;
const DEV_DEBOUNCE_MS: u32 = 350;

fn taskbar_created_msg() -> u32 {
    static ID: AtomicU32 = AtomicU32::new(0);
    let id = ID.load(Ordering::Relaxed);
    if id != 0 {
        return id;
    }
    let id = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    ID.store(id, Ordering::Relaxed);
    id
}

/// 宿主状态。由调用方 Box 唯一持有，窗口过程只借用。
pub struct HostState {
    /// 宿主窗口句柄；窗口创建完成后由调用方回填。
    pub hwnd: HWND,
    /// 业务状态（规则/设备/速度）——单线程直接拥有，无锁。
    pub app: AppState,
    /// 菜单模型（设备行/速度/自启/悬停/键盘焦点）。
    pub model: MenuModel,
    pub tray: Option<Tray>,
    /// 当前打开的菜单窗口。
    pub menu: Option<HWND>,
    /// 菜单是否成功激活过（失活自毁的豁免依据）。跨窗口过程访问，用原子量。
    pub menu_ever_active: std::sync::atomic::AtomicBool,
    /// 菜单窗口创建中（创建调用栈内不响应关闭请求）。
    pub menu_opening: bool,
    /// 菜单内的指针速度 Trackbar 滑块子控件。
    pub trackbar: Option<HWND>,
    /// 菜单内的滚轮速度 Trackbar 滑块子控件。
    pub wheel_trackbar: Option<HWND>,
    /// 设备子菜单内的指针速度 Trackbar（本地预览，不直接改系统）。
    pub sub_trackbar: Option<HWND>,
    /// 设备子菜单内的滚轮速度 Trackbar（本地预览）。
    pub sub_wheel_trackbar: Option<HWND>,
    /// 菜单字体（按菜单窗口 DPI 创建，菜单关闭时销毁）。
    pub font: windows::Win32::Graphics::Gdi::HFONT,
    /// 设备子菜单窗口。
    pub sub: Option<HWND>,
    /// 子菜单对应的设备行索引。
    pub sub_dev: Option<usize>,
    /// 子菜单内悬停的操作行（None = 信息行/无）。
    pub sub_hover_row: Option<usize>,
    /// 菜单内当前悬停目标。
    pub hover: Option<Hover>,
    /// 主菜单中按住的命令区（按下反馈；松手且未移出才触发动作）。
    pub pressed: Option<Hover>,
    /// 子菜单中按住的槽位（0..2 = 操作行，3 = 「设为规则」按钮）。
    pub sub_pressed: Option<usize>,
    /// 鼠标接口设备通知句柄（RAII）。
    pub dev_notify: Option<MouseDevNotify>,
    /// 系统「应用深色模式」与高对比度（WM_SETTINGCHANGE 时刷新）。
    pub theme_dark: bool,
    pub hc: bool,
    pub debug: bool,
}

impl HostState {
    /// `app` 需已加载配置、枚举设备并应用过规则（见 native 入口）。
    pub fn new(app: AppState) -> Box<HostState> {
        Box::new(HostState {
            hwnd: HWND::default(),
            app,
            model: MenuModel::new(speed::get(), speed::get_wheel(), crate::autostart::is_enabled()),
            tray: None,
            menu: None,
            menu_ever_active: std::sync::atomic::AtomicBool::new(false),
            menu_opening: false,
            trackbar: None,
            wheel_trackbar: None,
            sub_trackbar: None,
            sub_wheel_trackbar: None,
            font: windows::Win32::Graphics::Gdi::HFONT::default(),
            sub: None,
            sub_dev: None,
            sub_hover_row: None,
            hover: None,
            pressed: None,
            sub_pressed: None,
            dev_notify: None,
            theme_dark: menu::system_dark(),
            hc: menu::high_contrast(),
            debug: std::env::var("MSS_DEBUG_MENU").is_ok(),
        })
    }
}

/// 创建宿主窗口并把 `&mut HostState` 以借用指针接入。
/// 返回窗口句柄；宿主 hwnd 字段由调用方回填。
pub fn create_host_window(state: &mut HostState) -> Result<HWND, windows::core::Error> {
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    register_host_class(hinstance);
    let ptr = state as *mut HostState;
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            HOST_CLASS,
            w!("MouseSpeedSwitcher Host"),
            win::WINDOW_STYLE(0), // 真正隐藏的顶层窗口（不 ShowWindow）
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            Some(ptr.cast()),
        )
    }
}

fn register_host_class(hinstance: HINSTANCE) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(host_wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: HOST_CLASS,
            ..Default::default()
        };
        assert_ne!(RegisterClassExW(&wc), 0, "register host class");
    });
}

/// 阻塞式消息循环；收到 WM_QUIT（GetMessageW 返回 0）后返回退出码。
pub fn message_loop() -> i32 {
    let mut msg = MSG::default();
    unsafe {
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == -1 {
                continue;
            }
            if !r.as_bool() {
                return msg.wParam.0 as i32;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn host_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // WM_NCCREATE：只存指针，交还 DefWindowProc 完成创建
    if msg == WM_NCCREATE {
        let cs = lp.0 as *const CREATESTRUCTW;
        let state = (*cs).lpCreateParams as *mut HostState;
        SetWindowLongPtrW(hwnd, win::GWLP_USERDATA, state as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ptr = GetWindowLongPtrW(hwnd, win::GWLP_USERDATA) as *mut HostState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let state = &mut *ptr;

    match msg {
        tray::WM_APP_TRAY => {
            if Tray::is_click_up(lp.0) {
                toggle_menu(state);
            }
            LRESULT(0)
        }
        m if m == taskbar_created_msg() => {
            // Explorer 重启：重新添加托盘图标
            if let Some(t) = state.tray.as_mut() {
                let tip = t.last_tip().to_string();
                t.remove();
                if !t.add(&tip) && state.debug {
                    eprintln!("[mss-debug] TaskbarCreated: re-add failed");
                }
            }
            LRESULT(0)
        }
        WM_APP_CLOSE_MENU => {
            if state.debug {
                eprintln!("[mss-debug] host got close request, menu={:?} opening={}", state.menu.is_some(), state.menu_opening);
            }
            close_menu(state);
            LRESULT(0)
        }
        WM_APP_CLOSE_SUBMENU => {
            // 处理时重新检查：指针可能已回到设备行/子菜单（硬件消息先于本投递处理）
            if state.model.sub_hover.is_none() {
                submenu::close(state);
            }
            LRESULT(0)
        }
        WM_APP_SUB_LEFT => {
            // 子菜单滑块的 leave：光标可能从滑块直接移出子菜单窗口，
            // 也可能只是移回子菜单窗体内部（父窗口 WM_MOUSEMOVE 会接管）
            let inside = match state.sub {
                Some(h) => submenu::cursor_inside_window(h),
                None => false,
            };
            if inside {
                state.model.sub_pointer_inside = true;
            } else {
                state.sub_pressed = None;
                state.model.sub_pointer_inside = false;
                state.sub_hover_row = None;
                if !matches!(state.hover, Some(Hover::Device(_))) {
                    state.model.sub_hover = None;
                }
                if state.model.sub_hover.is_none() {
                    submenu::close(state);
                } else {
                    invalidate_menu(state);
                }
            }
            LRESULT(0)
        }
        win::WM_DEVICECHANGE => {
            // 只关心可能影响设备集合的事件；防抖后统一扫描一次
            let ev = wp.0 as u32;
            if ev == win::DBT_DEVICEARRIVAL
                || ev == win::DBT_DEVICEREMOVECOMPLETE
                || ev == win::DBT_DEVNODES_CHANGED
            {
                SetTimer(Some(hwnd), TIMER_DEV, DEV_DEBOUNCE_MS, None);
            }
            LRESULT(1)
        }
        win::WM_TIMER if wp.0 == TIMER_DEV => {
            let _ = KillTimer(Some(hwnd), TIMER_DEV);
            debounced_scan(state);
            LRESULT(0)
        }
        win::WM_SETTINGCHANGE => {
            // 系统设置改了指针/滚轮速度 → 同步滑块/菜单/tooltip
            let cur = speed::get();
            if cur != state.model.speed_val {
                state.model.speed_val = cur;
                if let Some(tb) = state.trackbar {
                    SendMessageW(tb, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(cur as isize)));
                }
                invalidate_menu(state);
            }
            let cur_wheel = speed::get_wheel();
            if cur_wheel != state.model.wheel_val {
                state.model.wheel_val = cur_wheel;
                if let Some(tb) = state.wheel_trackbar {
                    SendMessageW(tb, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(cur_wheel as isize)));
                }
                invalidate_menu(state);
            }
            if cur != state.model.speed_val || cur_wheel != state.model.wheel_val {
                sync_tip(state);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            state.tray.take(); // Drop 内部 NIM_DELETE + DestroyIcon
            state.menu = None;
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 宿主窗口句柄（菜单模块经 GWLP_USERDATA 反查的便捷封装）。
pub fn host_hwnd(ptr: *mut HostState) -> HWND {
    unsafe { (*ptr).hwnd }
}

// ── 低级鼠标钩子（菜单打开期间点击外部 → 关闭）──────────────
//
// 失焦关闭依赖 WM_ACTIVATE(WA_INACTIVE)，但托盘弹窗的前台锁时序在部分
// 机器上仍会漏（AttachThreadInput 修复后实测仍复现）。钩子不依赖激活
// 状态：菜单打开期间任何落在菜单/子菜单窗口之外的按下都投递关闭请求，
// 事件本身照常放行（CallNextHookEx），不影响其它程序的交互。
// 钩子回调与消息循环同线程（系统经消息泵回调），可直接用静态量。

static HOOK_HOST: AtomicIsize = AtomicIsize::new(0);
static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// 安装 WH_MOUSE_LL 钩子（仅菜单打开期间存在）。
pub fn install_mouse_close_hook(host: HWND) {
    if HOOK_HANDLE.load(Ordering::Relaxed) != 0 {
        return;
    }
    match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), None, 0) } {
        Ok(h) => {
            HOOK_HANDLE.store(h.0 as isize, Ordering::Release);
            HOOK_HOST.store(host.0 as isize, Ordering::Release);
        }
        Err(_) => {
            // 钩子装不上（罕见）：退回纯失活关闭路径，不致命
        }
    }
}

/// 卸载钩子。
pub fn remove_mouse_close_hook() {
    let h = HOOK_HANDLE.swap(0, Ordering::AcqRel);
    HOOK_HOST.store(0, Ordering::Release);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut c_void));
        }
    }
}

/// 命中窗口的根窗口是否为本程序的弹出菜单/子菜单（按窗口类名判断）。
fn is_mss_popup(root: HWND) -> bool {
    let mut buf = [0u16; 16];
    let n = unsafe { GetClassNameW(root, &mut buf) };
    if n <= 0 {
        return false;
    }
    let name = &buf[..n as usize];
    unsafe {
        name == w!("MSS_Menu").as_wide() || name == w!("MSS_SubMenu").as_wide()
    }
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wp.0 as u32;
        if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN {
            let host = HOOK_HOST.load(Ordering::Acquire);
            if host != 0 {
                let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
                let pt = POINT { x: info.pt.x, y: info.pt.y };
                let hit = WindowFromPoint(pt);
                let root = if hit.is_invalid() {
                    HWND::default()
                } else {
                    GetAncestor(hit, GA_ROOT)
                };
                if !root.is_invalid() && !is_mss_popup(root) {
                    let _ = PostMessageW(
                        Some(HWND(host as *mut c_void)),
                        WM_APP_CLOSE_MENU,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// 托盘点击：有菜单则关闭，无则打开。
fn toggle_menu(state: &mut HostState) {
    if state.menu_opening {
        return;
    }
    if state.menu.is_some() {
        unsafe { let _ = PostMessageW(Some(state.hwnd), WM_APP_CLOSE_MENU, WPARAM(0), LPARAM(0)); }
    } else {
        open_menu(state);
    }
}

fn open_menu(state: &mut HostState) {
    state.menu_opening = true;
    let r = menu::open(state);
    state.menu_opening = false;
    match r {
        Ok(hwnd) => {
            state.menu = Some(hwnd);
            state.menu_ever_active.store(false, Ordering::Relaxed);
            state.pressed = None;
            state.sub_pressed = None;
            install_mouse_close_hook(state.hwnd);
        }
        Err(e) => {
            if state.debug {
                eprintln!("[mss-debug] open menu failed: {e}");
            }
        }
    }
}

/// 关闭菜单：先从宿主状态摘除，再销毁窗口；把焦点还给通知区域。
/// 注意菜单的 WM_DESTROY 不得反向访问宿主状态（见模块注释）。
pub fn close_menu(state: &mut HostState) {
    if state.menu_opening {
        return; // 创建调用栈内不销毁（踩坑 三-3）
    }
    remove_mouse_close_hook();
    state.pressed = None;
    state.sub_pressed = None;
    if let Some(h) = state.menu.take() {
        state.trackbar = None;
        state.wheel_trackbar = None;
        state.sub_trackbar = None;
        state.sub_wheel_trackbar = None;
        state.hover = None;
        if !state.font.0.is_null() {
            unsafe {
                let _ = DeleteObject(state.font.into());
            }
            state.font = windows::Win32::Graphics::Gdi::HFONT::default();
        }
        state.sub = None; // 子菜单是菜单的 ownee，随级联销毁
        state.sub_dev = None;
        state.sub_hover_row = None;
        unsafe {
            let r = DestroyWindow(h);
            if state.debug {
                eprintln!("[mss-debug] DestroyWindow(menu) ok={}", r.is_ok());
            }
        }
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: state.hwnd,
            uID: tray::uid(),
            ..Default::default()
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_SETFOCUS, &mut nid);
        }
    }
    state.menu_ever_active.store(false, Ordering::Relaxed);
}

/// 防抖到期：只做一次全量枚举 + 差异应用，再同步 tooltip 与菜单显示。
fn debounced_scan(state: &mut HostState) {
    let mice = devices::enumerate_mice();
    state.app.apply_diff(&mice);
    let changed = state.model.devs.len() != mice.len();
    state.model.devs = menu_model::build_dev_rows(&mice, &state.app);
    state.model.effective = menu_model::effective_name(&state.app);
    if state.debug {
        eprintln!("[mss-debug] dev scan: {} mice", mice.len());
    }
    if changed {
        // 设备数变化后子菜单的设备索引可能失效，先关掉
        submenu::close(state);
        state.model.sub_hover = None;
    }
    invalidate_menu(state);
    sync_tip(state);
}

/// 菜单打开中则整体重绘。
fn invalidate_menu(state: &HostState) {
    if let Some(h) = state.menu {
        unsafe {
            let _ = InvalidateRect(Some(h), None, false);
        }
    }
}

/// 依当前速度与生效规则生成 tooltip；Tray 内部按文本去重后 NIM_MODIFY。
pub fn sync_tip(state: &mut HostState) {
    let rule = menu_model::effective_name(&state.app);
    let tip = menu_model::tip_text(
        speed::get(),
        speed::get_wheel(),
        rule.as_ref().map(|(n, s)| (n.as_str(), *s)),
    );
    if let Some(t) = state.tray.as_mut() {
        t.set_tip(&tip);
    }
}
