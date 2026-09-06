//! egui 版自绘托盘菜单。
//!
//! 菜单是常驻透明无框窗口：空闲时缩成 1×1 藏在屏幕外（保证消息循环持续
//! 运行以轮询托盘事件），点击托盘后移动到光标处并放大成菜单。布局模仿
//! 原生菜单：COLOR_MENU 背景、COLOR_HIGHLIGHT 悬停、左侧勾选列。
//!
//! 设备子菜单是独立原生子窗口（egui viewport），尺寸即子菜单本身，
//! 悬停设备行时弹出在菜单右侧——与原生菜单一样没有多余背板。子窗口
//! 不激活（不抢焦点），主菜单保持前台，失焦关闭逻辑不受影响。
//!
//! 交互与绘制抽在 [`MenuModel`]（不接触 Win32/eframe），可用 egui 合成
//! 输入做单元测试；[`App`] 只负责副作用（调速、托盘、开关菜单、退出）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{
    Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, ViewportCommand, WindowLevel,
};

use crate::{autostart, config, devices, speed, state::AppState};

// ── 颜色（模仿原生菜单）────────────────────────────────
const BG: Color32 = Color32::from_rgb(0xF0, 0xF0, 0xF0);
const BORDER: Color32 = Color32::from_rgb(0x9A, 0x9A, 0x9A);
const SEPARATOR: Color32 = Color32::from_rgb(0xD9, 0xD9, 0xD9);
const TEXT: Color32 = Color32::from_rgb(0x1F, 0x1F, 0x1F);
const GRAY: Color32 = Color32::from_rgb(0x6D, 0x6D, 0x6D);
const HIGHLIGHT: Color32 = Color32::from_rgb(0x00, 0x78, 0xD7);
const HIGHLIGHT_TEXT: Color32 = Color32::WHITE;

// ── 布局（逻辑点，DPI 由 egui 统一缩放）───────────────
const MENU_W: f32 = 320.0;
const SUB_W: f32 = 180.0;
const ROW_H: f32 = 26.0;
const SUB_ROW_H: f32 = 24.0;
const SEP_H: f32 = 8.0;
const TITLE_H: f32 = 26.0;
const SLIDER_H: f32 = 28.0;
const INFO_ROW_H: f32 = 22.0;
const TOP_PAD: f32 = 4.0;
const BOTTOM_PAD: f32 = 6.0;
const PAD: f32 = 8.0;
/// 勾选标记列宽。
const CHECK_W: f32 = 18.0;

// ── 纯数据与格式化（可单测）────────────────────────────

/// 一台设备的菜单行数据。
#[derive(Debug, Clone)]
pub struct DevRow {
    pub name: String,
    pub vid: Option<String>,
    pub pid: Option<String>,
    /// 当前规则速度（None = 无规则）。
    pub rule_speed: Option<u32>,
    /// 是否为当前生效规则设备。
    pub is_effective: bool,
}

impl DevRow {
    /// 设备行显示文本（与旧版一致）。
    pub fn row_text(&self) -> String {
        let vid = self.vid.as_deref().unwrap_or("----");
        let pid = self.pid.as_deref().unwrap_or("----");
        let mut s = format!("{}  [{}:{}]", self.name, vid, pid);
        if let Some(sp) = self.rule_speed {
            s.push_str(&format!("  · 规则 {sp}"));
        }
        s
    }

    /// 是否可配置规则（有 VID/PID）。
    pub fn can_rule(&self) -> bool {
        self.vid.is_some() && self.pid.is_some()
    }

    /// 子菜单三操作：(标签, 是否可点)。
    pub fn sub_actions(&self) -> [(&'static str, bool); 3] {
        [
            ("用当前速度保存规则", self.can_rule()),
            ("删除此设备规则", self.can_rule() && self.rule_speed.is_some()),
            ("重新应用规则", self.can_rule()),
        ]
    }

    /// 子菜单信息行（VID/PID）。
    pub fn sub_info(&self) -> String {
        let vid = self.vid.as_deref().unwrap_or("----");
        let pid = self.pid.as_deref().unwrap_or("----");
        format!("VID:{}  PID:{}", vid, pid)
    }
}

/// 托盘 tooltip 文本。
pub fn tip_text(cur: u32, rule: Option<(&str, u32)>) -> String {
    match rule {
        Some((name, sp)) => {
            format!("鼠标灵敏度切换 - 当前指针速度: {cur} · 生效规则: {name} (速度 {sp})")
        }
        None => format!("鼠标灵敏度切换 - 当前指针速度: {cur}"),
    }
}

/// 菜单窗口总高度（点）。
pub fn menu_height(n_devs: usize) -> f32 {
    let dev_rows = if n_devs == 0 { INFO_ROW_H } else { n_devs as f32 * ROW_H };
    TOP_PAD
        + TITLE_H
        + SLIDER_H
        + SEP_H
        + INFO_ROW_H
        + SEP_H
        + dev_rows
        + SEP_H
        + ROW_H
        + SEP_H
        + ROW_H
        + BOTTOM_PAD
}

/// 子菜单窗口高度（信息行 + 3 操作行）。
pub fn sub_height() -> f32 {
    4.0 + SUB_ROW_H * 4.0 + 4.0
}

/// 设备行布局：第 `idx` 行的菜单内 y 坐标起点（点）。
pub fn device_row_top(idx: usize) -> f32 {
    TOP_PAD + TITLE_H + SLIDER_H + SEP_H + INFO_ROW_H + SEP_H + idx as f32 * ROW_H
}

// ── 菜单模型（与 OS 无关，可用 egui 合成输入单测）──────

/// 菜单交互动作（由 [`MenuModel`] 绘制产出，App 负责执行副作用）。
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    SetSpeed(u32),
    ResetSpeed,
    ToggleAutostart,
    SetRule(usize),
    DelRule(usize),
    Reapply(usize),
    Exit,
}

/// 菜单的全部交互状态与绘制。
pub struct MenuModel {
    pub devs: Vec<DevRow>,
    pub effective: Option<(String, u32)>,
    pub autostart_on: bool,
    pub speed_val: u32,
    /// 悬停的设备行（索引 + 行矩形，菜单视口点坐标）。Some = 子菜单应显示。
    pub sub_hover: Option<(usize, Rect)>,
    /// 上一帧子菜单窗口回报的「指针在子菜单内」。
    /// 光标在菜单与子菜单之间移动时靠它保持子菜单不闪关。
    pub sub_pointer_inside: bool,
}

impl MenuModel {
    /// 绘制主菜单，返回本帧产生的动作。
    /// 子菜单由 [`Self::draw_submenu`] 在独立窗口中绘制（App 调度）。
    pub fn draw(&mut self, ui: &mut egui::Ui) -> Vec<MenuAction> {
        let h = menu_height(self.devs.len());
        let mut actions: Vec<MenuAction> = Vec::new();

        let painter = ui.painter().clone();
        let menu_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(MENU_W, h));
        painter.rect_filled(menu_rect, 2.0, BG);
        painter.rect_stroke(menu_rect, 2.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);

        let mut y = TOP_PAD;

        // ── 标题行：指针速度: N + 恢复默认 ──
        let title_rect =
            Rect::from_min_size(egui::pos2(PAD, y), egui::vec2(MENU_W - 2.0 * PAD, TITLE_H));
        {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(title_rect));
            child.horizontal_centered(|ui| {
                ui.label(
                    egui::RichText::new(format!("指针速度: {}", self.speed_val))
                        .color(TEXT)
                        .size(14.0),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("恢复默认").clicked() {
                        actions.push(MenuAction::ResetSpeed);
                    }
                });
            });
        }
        y += TITLE_H;

        // ── 滑动条 ──
        let sl_rect =
            Rect::from_min_size(egui::pos2(PAD, y), egui::vec2(MENU_W - 2.0 * PAD, SLIDER_H));
        {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(sl_rect));
            child.set_height(SLIDER_H);
            child.centered_and_justified(|ui| {
                let mut tmp = self.speed_val as i32;
                let resp = ui.add(
                    egui::Slider::new(&mut tmp, 1..=20)
                        .integer()
                        .show_value(false)
                        .smart_aim(false),
                );
                if resp.changed() {
                    self.speed_val = tmp.clamp(1, 20) as u32;
                    actions.push(MenuAction::SetSpeed(self.speed_val));
                }
            });
        }
        y += SLIDER_H;

        y = draw_sep(&painter, y, MENU_W);

        // ── 生效规则行（固定显示）──
        let eff_text = match &self.effective {
            Some((n, s)) => format!("生效规则: {n} (速度 {s})"),
            None => "生效规则: 无".to_string(),
        };
        painter.text(
            egui::pos2(PAD, y + INFO_ROW_H / 2.0),
            Align2::LEFT_CENTER,
            eff_text,
            FontId::proportional(13.0),
            GRAY,
        );
        y += INFO_ROW_H;
        y = draw_sep(&painter, y, MENU_W);

        // ── 设备行 ──
        let mut hovered_dev: Option<(usize, Rect)> = None;
        if self.devs.is_empty() {
            painter.text(
                egui::pos2(PAD, y + INFO_ROW_H / 2.0),
                Align2::LEFT_CENTER,
                "未检测到鼠标设备",
                FontId::proportional(13.0),
                GRAY,
            );
            y += INFO_ROW_H;
        }
        for (i, d) in self.devs.iter().enumerate() {
            let r = Rect::from_min_size(egui::pos2(0.0, y), egui::vec2(MENU_W, ROW_H));
            let resp = ui.allocate_rect(r, Sense::hover());
            let hovered = resp.hovered();
            if hovered {
                painter.rect_filled(r, 0.0, HIGHLIGHT);
            }
            let color = if hovered { HIGHLIGHT_TEXT } else { TEXT };
            if d.is_effective && d.rule_speed.is_some() {
                draw_check(&painter, r, PAD, color);
            }
            painter.text(
                egui::pos2(PAD + CHECK_W, r.center().y),
                Align2::LEFT_CENTER,
                d.row_text(),
                FontId::proportional(13.0),
                color,
            );
            if hovered {
                hovered_dev = Some((i, r));
            }
            y += ROW_H;
        }
        y = draw_sep(&painter, y, MENU_W);

        // ── 开机自启 ──
        let auto_rect = Rect::from_min_size(egui::pos2(0.0, y), egui::vec2(MENU_W, ROW_H));
        let resp = ui.allocate_rect(auto_rect, Sense::click());
        let hovered = resp.hovered();
        if hovered {
            painter.rect_filled(auto_rect, 0.0, HIGHLIGHT);
        }
        let color = if hovered { HIGHLIGHT_TEXT } else { TEXT };
        if self.autostart_on {
            draw_check(&painter, auto_rect, PAD, color);
        }
        painter.text(
            egui::pos2(PAD + CHECK_W, auto_rect.center().y),
            Align2::LEFT_CENTER,
            "开机自启",
            FontId::proportional(13.0),
            color,
        );
        if resp.clicked() {
            actions.push(MenuAction::ToggleAutostart);
        }
        y += ROW_H;
        y = draw_sep(&painter, y, MENU_W);

        // ── 退出 ──
        let exit_rect = Rect::from_min_size(egui::pos2(0.0, y), egui::vec2(MENU_W, ROW_H));
        let resp = ui.allocate_rect(exit_rect, Sense::click());
        let hovered = resp.hovered();
        if hovered {
            painter.rect_filled(exit_rect, 0.0, HIGHLIGHT);
        }
        painter.text(
            egui::pos2(PAD + CHECK_W, exit_rect.center().y),
            Align2::LEFT_CENTER,
            "退出",
            FontId::proportional(13.0),
            if hovered { HIGHLIGHT_TEXT } else { TEXT },
        );
        if resp.clicked() {
            actions.push(MenuAction::Exit);
        }

        // ── 子菜单开合：悬停设备行即开；光标进入子菜单后菜单收不到
        // 移动事件，靠 sub_pointer_inside 保持；两者皆无则关闭。 ──
        self.sub_hover = match hovered_dev {
            Some(h) => Some(h),
            None if self.sub_pointer_inside => self.sub_hover,
            None => None,
        };

        actions
    }

    /// 绘制设备子菜单（在独立视口窗口中，Ui 即整个子菜单窗口）。
    /// 返回 (动作, 指针是否在子菜单内)。
    pub fn draw_submenu(&mut self, ui: &mut egui::Ui, idx: usize) -> (Vec<MenuAction>, bool) {
        let mut actions: Vec<MenuAction> = Vec::new();
        let pointer_inside = ui.input(|i| i.pointer.latest_pos().is_some());

        let painter = ui.painter().clone();
        let srect = Rect::from_min_size(Pos2::ZERO, egui::vec2(SUB_W, sub_height()));
        painter.rect_filled(srect, 2.0, BG);
        painter.rect_stroke(srect, 2.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);

        if let Some(d) = self.devs.get(idx) {
            let mut sy = 4.0;
            painter.text(
                egui::pos2(PAD, sy + SUB_ROW_H / 2.0),
                Align2::LEFT_CENTER,
                d.sub_info(),
                FontId::proportional(12.0),
                GRAY,
            );
            sy += SUB_ROW_H;
            for (slot, (label, enabled)) in d.sub_actions().into_iter().enumerate() {
                let rr = Rect::from_min_size(
                    egui::pos2(2.0, sy),
                    egui::vec2(SUB_W - 4.0, SUB_ROW_H),
                );
                let resp =
                    ui.allocate_rect(rr, if enabled { Sense::click() } else { Sense::hover() });
                let hov = enabled && resp.hovered();
                if hov {
                    painter.rect_filled(rr, 0.0, HIGHLIGHT);
                }
                let color = if hov {
                    HIGHLIGHT_TEXT
                } else if !enabled {
                    GRAY
                } else {
                    TEXT
                };
                painter.text(
                    egui::pos2(PAD, rr.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(13.0),
                    color,
                );
                if enabled && resp.clicked() {
                    actions.push(match slot {
                        0 => MenuAction::SetRule(idx),
                        1 => MenuAction::DelRule(idx),
                        _ => MenuAction::Reapply(idx),
                    });
                }
                sy += SUB_ROW_H;
            }
        }

        (actions, pointer_inside)
    }
}

// ── 应用 ───────────────────────────────────────────────

/// 托盘事件线程与 UI 线程之间的点击信号。
pub static TRAY_CLICK: AtomicBool = AtomicBool::new(false);

/// 设备子菜单视口 ID。
fn sub_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("device_submenu")
}

pub struct App {
    app_state: Arc<Mutex<Option<AppState>>>,
    tray: crate::tray::TrayHandle,
    /// 后台轮询发现设备变化时置位 → 菜单打开状态下刷新数据。
    dirty: Arc<AtomicBool>,
    tip_rx: std::sync::mpsc::Receiver<String>,
    last_tip: String,

    model: MenuModel,
    menu_open: bool,
    ever_focused: bool,
    /// 当前窗口尺寸（点），防重复发尺寸命令。
    cur_size: egui::Vec2,
    debug: bool,
    debug_opened: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        app_state: Arc<Mutex<Option<AppState>>>,
        tray: crate::tray::TrayHandle,
        dirty: Arc<AtomicBool>,
        tip_rx: std::sync::mpsc::Receiver<String>,
    ) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);
        let debug = std::env::var("MSS_DEBUG_MENU").is_ok();
        Self {
            app_state,
            tray,
            dirty,
            tip_rx,
            last_tip: String::new(),
            model: MenuModel {
                devs: Vec::new(),
                effective: None,
                autostart_on: autostart::is_enabled(),
                speed_val: speed::get(),
                sub_hover: None,
                sub_pointer_inside: false,
            },
            menu_open: false,
            ever_focused: false,
            cur_size: egui::Vec2::new(1.0, 1.0),
            debug,
            debug_opened: false,
        }
    }

    // ── 数据 ──

    /// 重新枚举设备并构建菜单数据（等价旧版 show_tray_menu 的组装逻辑）。
    fn refresh_data(&mut self) {
        let mice = devices::enumerate_mice();
        let guard = self.app_state.lock().unwrap();
        let effective: Option<(String, String)> = guard
            .as_ref()
            .and_then(|s| s.effective_rule())
            .and_then(|(d, _)| {
                let v = d.vid.clone()?;
                let p = d.pid.clone()?;
                Some((v, p))
            });
        self.model.devs = mice
            .iter()
            .map(|dev| {
                let rule_speed = dev
                    .vid
                    .as_deref()
                    .zip(dev.pid.as_deref())
                    .and_then(|(v, p)| {
                        guard
                            .as_ref()
                            .and_then(|s| s.cfg.rule_for(v, p))
                            .map(|r| r.speed)
                    });
                let is_effective = rule_speed.is_some()
                    && effective
                        .as_ref()
                        .map(|(ev, ep)| {
                            ev == dev.vid.as_deref().unwrap_or("")
                                && ep == dev.pid.as_deref().unwrap_or("")
                        })
                        .unwrap_or(false);
                DevRow {
                    name: dev.name.clone(),
                    vid: dev.vid.clone(),
                    pid: dev.pid.clone(),
                    rule_speed,
                    is_effective,
                }
            })
            .collect();
        self.model.effective = guard
            .as_ref()
            .and_then(|s| s.effective_rule())
            .map(|(d, sp)| (d.name.clone(), sp));
    }

    // ── 开合 ──

    fn open_menu(&mut self, ctx: &egui::Context) {
        self.refresh_data();
        self.model.speed_val = speed::get();
        self.model.autostart_on = autostart::is_enabled();
        self.model.sub_hover = None;
        self.model.sub_pointer_inside = false;
        self.ever_focused = false;
        self.menu_open = true;

        let h = menu_height(self.model.devs.len());
        self.set_window_size(ctx, egui::Vec2::new(MENU_W, h));
        // 光标处弹出；按「菜单+子菜单」全宽夹取，保证子菜单展开后不出屏
        let ppp = ctx.pixels_per_point();
        let (cx, cy) = cursor_pos();
        let w_px = (MENU_W + SUB_W) * ppp;
        let h_px = h * ppp;
        let (x, y) = clamp_to_work_area(cx, cy, w_px, h_px);
        if self.debug {
            eprintln!("[mss-debug] open: cursor=({cx},{cy}) pos=({x},{y}) ppp={ppp}");
        }
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(egui::pos2(x as f32 / ppp, y as f32 / ppp)));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn close_menu(&mut self, ctx: &egui::Context) {
        self.menu_open = false;
        self.model.sub_hover = None;
        self.model.sub_pointer_inside = false;
        // 缩回 1×1 藏到屏幕外，并把焦点还给系统外壳窗口
        self.set_window_size(ctx, egui::Vec2::new(1.0, 1.0));
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(egui::pos2(-32000.0, -32000.0)));
        release_focus();
        if self.debug {
            eprintln!("[mss-debug] closed");
        }
    }

    fn set_window_size(&mut self, ctx: &egui::Context, size: egui::Vec2) {
        if (self.cur_size - size).abs().max_elem() > 0.5 {
            self.cur_size = size;
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        }
    }

    // ── 动作 ──

    fn run_action(&mut self, ctx: &egui::Context, action: MenuAction) {
        match action {
            MenuAction::SetSpeed(v) => speed::set(v),
            MenuAction::ResetSpeed => {
                self.model.speed_val = 10;
                speed::set(10);
            }
            MenuAction::ToggleAutostart => {
                if autostart::is_enabled() {
                    autostart::disable();
                } else {
                    autostart::enable();
                }
                self.model.autostart_on = autostart::is_enabled();
                // 不关闭菜单（与旧版一致）
            }
            MenuAction::SetRule(i) => {
                if let Some(d) = self.model.devs.get(i) {
                    if let (Some(v), Some(p)) = (d.vid.clone(), d.pid.clone()) {
                        let cur = speed::get();
                        {
                            let mut guard = self.app_state.lock().unwrap();
                            if let Some(st) = guard.as_mut() {
                                st.cfg.set_rule(&v, &p, cur, Some(d.name.clone()));
                                let _ = config::save(&st.cfg);
                                st.reapply();
                            }
                        }
                    }
                }
                self.close_menu(ctx);
                self.sync_tip_now();
            }
            MenuAction::DelRule(i) => {
                if let Some(d) = self.model.devs.get(i) {
                    if let (Some(v), Some(p)) = (d.vid.clone(), d.pid.clone()) {
                        {
                            let mut guard = self.app_state.lock().unwrap();
                            if let Some(st) = guard.as_mut() {
                                st.cfg.remove_rule(&v, &p);
                                let _ = config::save(&st.cfg);
                                st.reapply();
                            }
                        }
                    }
                }
                self.close_menu(ctx);
                self.sync_tip_now();
            }
            MenuAction::Reapply(_) => {
                {
                    let mut guard = self.app_state.lock().unwrap();
                    if let Some(st) = guard.as_mut() {
                        st.reapply();
                    }
                }
                self.close_menu(ctx);
                self.sync_tip_now();
            }
            MenuAction::Exit => {
                self.menu_open = false;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// 立即同步一次托盘 tooltip。
    fn sync_tip_now(&mut self) {
        let rule = self
            .app_state
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.effective_rule())
            .map(|(d, sp)| (d.name.clone(), sp));
        let tip = tip_text(speed::get(), rule.as_ref().map(|(n, s)| (n.as_str(), *s)));
        self.tray.set_tip(&tip);
        self.last_tip = tip;
    }

    // ── 绘制 ──

    fn draw_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let h = menu_height(self.model.devs.len());
        self.set_window_size(&ctx, egui::Vec2::new(MENU_W, h));

        let actions = self.model.draw(ui);

        // MSS_DEBUG_SUB=1：强制展开设备 0 的子菜单（验证独立视口窗口）。
        // 在 draw() 之后施加：draw() 会按真实悬停状态重算 sub_hover。
        if self.debug && std::env::var("MSS_DEBUG_SUB").is_ok() {
            self.model.sub_hover = Some((
                0,
                Rect::from_min_size(
                    egui::pos2(0.0, device_row_top(0)),
                    egui::vec2(MENU_W, ROW_H),
                ),
            ));
        }

        for action in actions {
            self.run_action(&ctx, action);
            if !self.menu_open {
                return;
            }
        }

        // ── 设备子菜单：独立原生窗口，紧贴菜单右缘、顶对齐悬停行 ──
        if let Some((idx, row)) = self.model.sub_hover {
            let outer = ctx
                .input(|i| i.viewport().outer_rect)
                .unwrap_or(Rect::from_min_size(Pos2::ZERO, egui::vec2(MENU_W, 274.0)));
            let sub_pos = egui::pos2(outer.left() + MENU_W, outer.top() + row.top());
            let builder = egui::ViewportBuilder::default()
                .with_decorations(false)
                .with_resizable(false)
                .with_active(false)
                .with_taskbar(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_position(sub_pos)
                .with_inner_size(egui::vec2(SUB_W, sub_height()));
            let (sub_actions, inside) = ctx.show_viewport_immediate(
                sub_viewport_id(),
                builder,
                |ui, _class| self.model.draw_submenu(ui, idx),
            );
            self.model.sub_pointer_inside = inside;
            for action in sub_actions {
                self.run_action(&ctx, action);
                if !self.menu_open {
                    return;
                }
            }
        }
    }

    /// 空闲轮询：托盘点击 / tooltip 更新 / 设备变化 / Esc / 失焦。
    /// 窗口隐藏时 eframe 也会因 request_repaint 调用本方法。
    fn logic(&mut self, ctx: &egui::Context) {
        ctx.request_repaint_after(Duration::from_millis(100));

        // MSS_DEBUG_MENU=1：启动即弹一次菜单（自动化冒烟测试用）
        if self.debug && !self.debug_opened {
            self.debug_opened = true;
            self.open_menu(ctx);
            return;
        }


        // 托盘 tooltip 更新
        while let Ok(tip) = self.tip_rx.try_recv() {
            if tip != self.last_tip {
                self.tray.set_tip(&tip);
                self.last_tip = tip;
            }
        }

        // 后台发现设备变化 → 菜单打开中则刷新数据
        if self.menu_open && self.dirty.swap(false, Ordering::Relaxed) {
            self.refresh_data();
        }

        // 托盘点击（由 tray 事件线程置位）
        if TRAY_CLICK.swap(false, Ordering::Relaxed) {
            self.open_menu(ctx);
            return;
        }

        if self.menu_open {
            // Esc 关闭
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.close_menu(ctx);
                return;
            }
            // 失焦自动关闭（仅在一次成功获得焦点之后）。
            // MSS_DEBUG_MENU 调试模式下跳过：锁屏环境焦点会被系统收回。
            let focused = ctx.input(|i| i.viewport().focused);
            match focused {
                Some(true) => self.ever_focused = true,
                Some(false) if self.ever_focused && !self.debug => {
                    self.close_menu(ctx);
                }
                _ => {}
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        App::logic(self, ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.menu_open {
            return;
        }
        self.draw_menu(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // 窗口透明：菜单区域之外不显示任何底色
        [0.0, 0.0, 0.0, 0.0]
    }
}

// ── 绘制辅助 ──

fn draw_sep(painter: &egui::Painter, y: f32, w: f32) -> f32 {
    let mid = y + SEP_H / 2.0;
    painter.line_segment(
        [egui::pos2(PAD, mid), egui::pos2(w - PAD, mid)],
        Stroke::new(1.0, SEPARATOR),
    );
    y + SEP_H
}

fn draw_check(painter: &egui::Painter, row: Rect, pad: f32, color: Color32) {
    let cx = row.left() + pad + CHECK_W / 2.0;
    let cy = row.center().y;
    let u = 4.0;
    painter.add(egui::Shape::line(
        vec![
            egui::pos2(cx - u, cy),
            egui::pos2(cx - u / 3.0, cy + u),
            egui::pos2(cx + u, cy - u),
        ],
        Stroke::new(2.0, color),
    ));
}

// ── 字体与样式 ──

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // 中文字形：运行时加载系统微软雅黑（失败则退回内置字体，仅显示方框不崩溃）
    for path in ["C:\\Windows\\Fonts\\msyh.ttc", "C:\\Windows\\Fonts\\msyh.ttf"] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("msyh".into(), egui::FontData::from_owned(bytes).into());
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push("msyh".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    let mut style = egui::Style::default();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(TEXT);
    style.spacing.item_spacing = egui::vec2(4.0, 2.0);
    ctx.set_style_of(egui::Theme::Light, style);
}

// ── Win32 辅助（光标 / 工作区 / 释放焦点）──

fn cursor_pos() -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

/// 把 (x,y) 夹取到光标所在显示器的工作区内，使 w×h 的窗口完整可见。
fn clamp_to_work_area(x: i32, y: i32, w: f32, h: f32) -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    let pt = POINT { x, y };
    let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        let nx = (x as f32).clamp(
            mi.rcWork.left as f32,
            (mi.rcWork.right as f32 - w).max(mi.rcWork.left as f32),
        );
        let ny = (y as f32).clamp(
            mi.rcWork.top as f32,
            (mi.rcWork.bottom as f32 - h).max(mi.rcWork.top as f32),
        );
        (nx as i32, ny as i32)
    } else {
        (x, y)
    }
}

/// 菜单关闭后把焦点交还系统外壳（窗口仍存在但藏在屏幕外，不占焦点）。
fn release_focus() {
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, SetForegroundWindow};
    unsafe {
        let shell = GetShellWindow();
        if !shell.is_invalid() {
            let _ = SetForegroundWindow(shell);
        }
    }
}

// ── 单元测试 ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, vid: Option<&str>, pid: Option<&str>, rule: Option<u32>) -> DevRow {
        DevRow {
            name: name.into(),
            vid: vid.map(Into::into),
            pid: pid.map(Into::into),
            rule_speed: rule,
            is_effective: false,
        }
    }

    #[test]
    fn device_row_text() {
        let d = dev("轨迹球", Some("046D"), Some("C52B"), Some(4));
        assert_eq!(d.row_text(), "轨迹球  [046D:C52B]  · 规则 4");
        assert!(d.can_rule());

        let blind = dev("盲设备", None, None, None);
        assert_eq!(blind.row_text(), "盲设备  [----:----]");
        assert!(!blind.can_rule());
    }

    #[test]
    fn effective_tip_and_header() {
        assert_eq!(tip_text(7, None), "鼠标灵敏度切换 - 当前指针速度: 7");
        assert_eq!(
            tip_text(4, Some(("轨迹球", 4))),
            "鼠标灵敏度切换 - 当前指针速度: 4 · 生效规则: 轨迹球 (速度 4)"
        );
    }

    #[test]
    fn sub_action_enable_rules() {
        let full = dev("轨迹球", Some("046D"), Some("C52B"), Some(4));
        let a = full.sub_actions();
        assert_eq!(a[0], ("用当前速度保存规则", true));
        assert_eq!(a[1], ("删除此设备规则", true));
        assert_eq!(a[2], ("重新应用规则", true));
        assert_eq!(full.sub_info(), "VID:046D  PID:C52B");

        let no_rule = dev("鼠标", Some("1"), Some("2"), None);
        let a = no_rule.sub_actions();
        assert!(a[0].1, "保存规则仍可点");
        assert!(!a[1].1, "无规则时删除置灰");
        assert!(a[2].1);

        let blind = dev("盲设备", None, None, None);
        let a = blind.sub_actions();
        assert!(!a[0].1 && !a[1].1 && !a[2].1, "无 VID/PID 全部置灰");
        assert!(blind.sub_info().contains("----"));
    }

    #[test]
    fn menu_height_monotonic() {
        let h0 = menu_height(0);
        let h1 = menu_height(1);
        let h4 = menu_height(4);
        assert!(h0 < h1 && h1 < h4);
        // n=0 时占位行高为 INFO_ROW_H，每加一台设备多一行 ROW_H
        assert!((h1 - h0 - (ROW_H - INFO_ROW_H)).abs() < 1e-3);
        assert!((h4 - h1 - 3.0 * ROW_H).abs() < 1e-3);
    }

    // ── 合成输入交互测试：直接驱动 egui pass，验证自绘菜单交互语义 ──

    const SCREEN: (f32, f32) = (MENU_W, 274.0);
    fn sub_screen() -> (f32, f32) {
        (SUB_W, sub_height())
    }

    fn model_4devs() -> MenuModel {
        MenuModel {
            devs: vec![
                dev("鼠标A", Some("1111"), Some("2222"), Some(4)),
                dev("鼠标B", Some("3333"), Some("4444"), None),
                dev("盲设备", None, None, None),
                dev("鼠标C", Some("5555"), Some("6666"), Some(7)),
            ],
            effective: Some(("鼠标A".into(), 4)),
            autostart_on: true,
            speed_val: 10,
            sub_hover: None,
            sub_pointer_inside: false,
        }
    }

    /// 跑一帧主菜单绘制。`events` 为本帧注入的 egui 事件。
    fn frame(
        model: &mut MenuModel,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<MenuAction> {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(SCREEN.0, SCREEN.1),
            )),
            time: Some(1.0),
            events,
            ..Default::default()
        };
        ctx.begin_pass(raw);
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::NULL,
            egui::UiBuilder::new().max_rect(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(SCREEN.0, SCREEN.1),
            )),
        );
        let actions = model.draw(&mut ui);
        let mut out = ctx.end_pass();
        // 无渲染器：丢弃字体图集等未应用的纹理增量
        out.textures_delta.clear();
        actions
    }

    /// 跑一帧子菜单绘制。
    fn sub_frame(
        model: &mut MenuModel,
        ctx: &egui::Context,
        idx: usize,
        events: Vec<egui::Event>,
    ) -> (Vec<MenuAction>, bool) {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(sub_screen().0, sub_screen().1),
            )),
            time: Some(1.0),
            events,
            ..Default::default()
        };
        ctx.begin_pass(raw);
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::NULL,
            egui::UiBuilder::new().max_rect(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(sub_screen().0, sub_screen().1),
            )),
        );
        let r = model.draw_submenu(&mut ui, idx);
        let mut out = ctx.end_pass();
        out.textures_delta.clear();
        r
    }

    fn click_events(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    }

    #[test]
    fn sim_hover_device_opens_submenu_and_moves_close_it() {
        let ctx = egui::Context::default();
        let mut m = model_4devs();
        // 预热：egui 命中测试用上一帧注册的控件矩形，需先空跑两帧
        frame(&mut m, &ctx, vec![]);
        frame(&mut m, &ctx, vec![]);

        // 悬停设备行 0（y = 96..122pt 中心 109）
        frame(&mut m, &ctx, vec![egui::Event::PointerMoved(egui::pos2(60.0, 109.0))]);
        assert!(m.sub_hover.is_some(), "悬停设备行应打开子菜单");
        assert_eq!(m.sub_hover.unwrap().0, 0);

        // 光标移入子菜单（菜单收到 PointerGone），子菜单回报指针在内 → 保持
        m.sub_pointer_inside = true;
        frame(&mut m, &ctx, vec![egui::Event::PointerGone]);
        assert!(m.sub_hover.is_some(), "移入子菜单不应关闭");

        // 光标离开子菜单（子菜单回报指针已不在）→ 关闭
        m.sub_pointer_inside = false;
        frame(&mut m, &ctx, vec![egui::Event::PointerGone]);
        assert!(m.sub_hover.is_none(), "指针离开子菜单且不在设备行应关闭");

        // 移到自启行（非设备行）→ 无子菜单
        let auto_y = device_row_top(4) + 8.0 + ROW_H / 2.0;
        frame(&mut m, &ctx, vec![egui::Event::PointerMoved(egui::pos2(60.0, auto_y))]);
        assert!(m.sub_hover.is_none(), "非设备行无子菜单");
    }

    #[test]
    fn sim_click_sub_actions_respect_enabled() {
        let ctx = egui::Context::default();
        let mut m = model_4devs();
        // 预热：命中测试依赖上一帧注册的控件矩形
        sub_frame(&mut m, &ctx, 1, vec![]);
        sub_frame(&mut m, &ctx, 1, vec![]);

        // 子菜单行布局：信息行第 1 行，删除规则是第 3 行（第 2 个操作行）
        // 设备行 1 无规则：删除置灰 → 点击无动作
        let del_y = 4.0 + SUB_ROW_H * 2.5;
        let (a, _) = sub_frame(&mut m, &ctx, 1, click_events(egui::pos2(90.0, del_y)));
        assert!(
            !a.iter().any(|x| matches!(x, MenuAction::DelRule(_))),
            "置灰的删除规则不应产生动作: {a:?}"
        );

        // 设备行 0 有规则：删除规则可点（中间插空帧隔离上一次点击状态）
        sub_frame(&mut m, &ctx, 0, vec![]);
        let (a, _) = sub_frame(&mut m, &ctx, 0, click_events(egui::pos2(90.0, del_y)));
        assert!(
            a.contains(&MenuAction::DelRule(0)),
            "有规则时删除规则应可点: {a:?}"
        );

        // 无 VID/PID 的设备：三个操作全部置灰
        let (a, _) = sub_frame(&mut m, &ctx, 2, click_events(egui::pos2(90.0, 4.0 + SUB_ROW_H * 1.5)));
        assert!(
            !a.iter()
                .any(|x| matches!(x, MenuAction::SetRule(_) | MenuAction::DelRule(_) | MenuAction::Reapply(_))),
            "盲设备不应产生任何动作: {a:?}"
        );
    }

    #[test]
    fn sim_click_rows() {
        let ctx = egui::Context::default();
        let mut m = model_4devs();
        frame(&mut m, &ctx, vec![]);
        frame(&mut m, &ctx, vec![]);

        // 点击自启行 → ToggleAutostart
        let auto_y = device_row_top(4) + 8.0 + ROW_H / 2.0;
        let a = frame(&mut m, &ctx, click_events(egui::pos2(60.0, auto_y)));
        assert!(a.contains(&MenuAction::ToggleAutostart), "自启行点击: {a:?}");

        // 点击退出行 → Exit
        let exit_y = auto_y + ROW_H + SEP_H + ROW_H / 2.0;
        let a = frame(&mut m, &ctx, click_events(egui::pos2(60.0, exit_y)));
        assert!(a.contains(&MenuAction::Exit), "退出行点击: {a:?}");

        // 点击设备标题行：仅悬停展开，无命令动作
        let a = frame(&mut m, &ctx, click_events(egui::pos2(60.0, device_row_top(2) + 13.0)));
        assert!(
            !a.iter()
                .any(|x| matches!(x, MenuAction::SetRule(_) | MenuAction::DelRule(_) | MenuAction::Reapply(_))),
            "设备标题行点击不应产生设备命令: {a:?}"
        );
        assert!(m.sub_hover.is_some(), "但应悬停展开子菜单");
    }
}
