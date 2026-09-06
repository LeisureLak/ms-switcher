//! 纯菜单模型：与 OS / 绘制框架无关。
//!
//! 包含设备行数据、文本格式化、菜单布局（逻辑点）与交互状态
//! （悬停展开子菜单、点击行 → 动作）。egui 绘制层（app.rs）与将来的
//! 原生 Win32 层都只消费本模块，把输入事件映射为模型更新与 [`MenuAction`]。
//!
//! 本模块禁止依赖 egui / Win32，保证可脱离窗口环境单测。

// ── 布局（逻辑点，DPI 由各绘制层统一换算）──────────────
pub const MENU_W: f32 = 320.0;
pub const SUB_W: f32 = 240.0;
pub const ROW_H: f32 = 26.0;
pub const SUB_ROW_H: f32 = 24.0;
/// 子菜单滑块行高（容纳 Trackbar 子控件）。
pub const SUB_SLIDER_H: f32 = 30.0;
pub const SEP_H: f32 = 8.0;
pub const TITLE_H: f32 = 26.0;
pub const SLIDER_H: f32 = 28.0;
pub const INFO_ROW_H: f32 = 22.0;
pub const TOP_PAD: f32 = 4.0;
pub const BOTTOM_PAD: f32 = 6.0;
pub const PAD: f32 = 8.0;
/// 勾选标记列宽。
pub const CHECK_W: f32 = 18.0;
/// 规则图标列宽（小齿轮）。
pub const RULE_ICON_W: f32 = 18.0;

// ── 纯数据与格式化 ─────────────────────────────────────

use crate::devices::Device;
use crate::scroll::{TriggerBtn, SCROLL_PX_MAX, SCROLL_PX_MIN};
use crate::state::AppState;

/// 由设备清单与规则状态构建菜单设备行（UI 无关，两种 UI 共用）。
pub fn build_dev_rows(mice: &[Device], st: &AppState) -> Vec<DevRow> {
    let effective: Option<(String, String)> = st.effective_rule().and_then(|(d, _)| {
        let v = d.vid.clone()?;
        let p = d.pid.clone()?;
        Some((v, p))
    });
    mice.iter()
        .map(|dev| {
            let (rule_speed, rule_wheel) = dev
                .vid
                .as_deref()
                .zip(dev.pid.as_deref())
                .and_then(|(v, p)| st.cfg.rule_for(v, p).map(|r| (Some(r.speed), r.wheel)))
                .unwrap_or((None, None));
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
                rule_wheel,
                is_effective,
            }
        })
        .collect()
}

/// 当前生效规则（设备名, 规则速度），用于菜单行与 tooltip。
pub fn effective_name(st: &AppState) -> Option<(String, u32)> {
    st.effective_rule().map(|(d, sp)| (d.name.clone(), sp))
}

/// 一台设备的菜单行数据。
#[derive(Debug, Clone)]
pub struct DevRow {
    pub name: String,
    pub vid: Option<String>,
    pub pid: Option<String>,
    /// 当前规则速度（None = 无规则）。
    pub rule_speed: Option<u32>,
    /// 当前规则的滚轮速度（None = 无规则或规则不带滚轮）。
    pub rule_wheel: Option<u32>,
    /// 是否为当前生效规则设备。
    pub is_effective: bool,
}

impl DevRow {
    /// 设备行显示文本（规则速度不再此处显示，改由绘制层的小齿轮图标表示）。
    pub fn row_text(&self) -> String {
        let vid = self.vid.as_deref().unwrap_or("----");
        let pid = self.pid.as_deref().unwrap_or("----");
        format!("{}  [{}:{}]", self.name, vid, pid)
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
        format!("VID:{vid}  PID:{pid}")
    }
}

// ── 子菜单布局 ─────────────────────────────────────────
// 信息行 / 指针标签 / 指针滑块 / 滚轮标签 / 滚轮滑块 / 「设为规则」按钮 / 3 操作行

/// 子菜单信息行 y 起点（点）。
pub const SUB_INFO_TOP: f32 = 4.0;

/// 「指针速度: N」标签行 y 起点（点）。
pub fn sub_ptr_label_top() -> f32 {
    SUB_INFO_TOP + SUB_ROW_H
}

/// 子菜单指针 Trackbar 的 y 起点（点）。
pub fn sub_ptr_slider_top() -> f32 {
    sub_ptr_label_top() + SUB_ROW_H
}

/// 「滚轮速度: N」标签行 y 起点（点）。
pub fn sub_wheel_label_top() -> f32 {
    sub_ptr_slider_top() + SUB_SLIDER_H
}

/// 子菜单滚轮 Trackbar 的 y 起点（点）。
pub fn sub_wheel_slider_top() -> f32 {
    sub_wheel_label_top() + SUB_ROW_H
}

/// 「设为规则」按钮行 y 起点（点）。
pub fn sub_btn_top() -> f32 {
    sub_wheel_slider_top() + SUB_SLIDER_H
}

/// 子菜单第 `i` 个操作行的 y 起点（点）。
pub fn sub_action_top(i: usize) -> f32 {
    sub_btn_top() + SUB_ROW_H + i as f32 * SUB_ROW_H
}

/// 子菜单窗口高度（信息 + 双滑块 + 按钮 + 3 操作行，底 padding 4 点）。
pub fn sub_height() -> f32 {
    sub_action_top(3) + 4.0
}

/// 子菜单指针滑块矩形（全宽，DIP）。
pub fn sub_slider_rect() -> (f32, f32, f32, f32) {
    (
        PAD,
        sub_ptr_slider_top(),
        SUB_W - PAD,
        sub_ptr_slider_top() + SUB_SLIDER_H,
    )
}

/// 子菜单滚轮滑块矩形（全宽，DIP）。
pub fn sub_wheel_rect() -> (f32, f32, f32, f32) {
    (
        PAD,
        sub_wheel_slider_top(),
        SUB_W - PAD,
        sub_wheel_slider_top() + SUB_SLIDER_H,
    )
}

/// 子菜单「设为规则」按钮矩形（整行，DIP）。
pub fn sub_btn_rect() -> (f32, f32, f32, f32) {
    (PAD, sub_btn_top(), SUB_W - PAD, sub_btn_top() + SUB_ROW_H)
}

/// 子菜单内 (x, y) 命中的操作行：Some(0..=2) = 操作行；None = 其它区域。
pub fn sub_row_at(y: f32) -> Option<usize> {
    let i = ((y - sub_action_top(0)) / SUB_ROW_H).floor();
    if i >= 0.0 && i <= 2.0 {
        Some(i as usize)
    } else {
        None
    }
}

/// 子菜单内 (x, y) 是否命中「设为规则」按钮行。
pub fn sub_btn_at(x: f32, y: f32) -> bool {
    let (l, t, r, b) = sub_btn_rect();
    x >= l && x < r && y >= t && y < b
}

/// 托盘 tooltip 文本。
pub fn tip_text(cur: u32, wheel: u32, rule: Option<(&str, u32)>) -> String {
    match rule {
        Some((name, sp)) => format!(
            "鼠标灵敏度切换 - 指针: {cur} · 滚轮: {wheel} · 生效规则: {name} (速度 {sp})"
        ),
        None => format!("鼠标灵敏度切换 - 指针: {cur} · 滚轮: {wheel}"),
    }
}

// ── 布局计算 ───────────────────────────────────────────

/// 主菜单「滚轮速度」标签行的 y 起点（点）。
pub fn wheel_label_top() -> f32 {
    TOP_PAD + TITLE_H + SLIDER_H
}

/// 主菜单滚轮 Trackbar 的 y 起点（点）。
pub fn wheel_slider_top() -> f32 {
    wheel_label_top() + TITLE_H
}

// ── 滚轮模式区（滚轮滑块之下）：模式勾选行 + 触发行 + 灵敏度标签 + 灵敏度滑块 ──

/// 「滚轮模式」勾选行的 y 起点（点）。
pub fn scroll_mode_top() -> f32 {
    wheel_slider_top() + SLIDER_H + SEP_H
}

/// 「触发键」行的 y 起点（点）。
pub fn scroll_trigger_top() -> f32 {
    scroll_mode_top() + ROW_H
}

/// 「滚动灵敏度」标签行的 y 起点（点）。
pub fn scroll_sens_label_top() -> f32 {
    scroll_trigger_top() + ROW_H
}

/// 滚轮灵敏度 Trackbar 的 y 起点（点）。
pub fn scroll_sens_slider_top() -> f32 {
    scroll_sens_label_top() + TITLE_H
}

/// 「生效规则」信息行的 y 起点（点）。
pub fn eff_row_top() -> f32 {
    scroll_sens_slider_top() + SLIDER_H + SEP_H
}

/// 菜单窗口总高度（点）。
pub fn menu_height(n_devs: usize) -> f32 {
    let dev_rows = if n_devs == 0 { INFO_ROW_H } else { n_devs as f32 * ROW_H };
    TOP_PAD
        + TITLE_H
        + SLIDER_H
        + TITLE_H
        + SLIDER_H
        + SEP_H
        + ROW_H
        + ROW_H
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

/// 设备行布局：第 `idx` 行的菜单内 y 坐标起点（点）。
pub fn device_row_top(idx: usize) -> f32 {
    eff_row_top() + INFO_ROW_H + SEP_H + idx as f32 * ROW_H
}

/// 「开机自启」行的 y 起点（点）。
pub fn autostart_row_top(n_devs: usize) -> f32 {
    device_row_top(n_devs.max(1)) + SEP_H
}

/// 「退出」行的 y 起点（点）。
pub fn exit_row_top(n_devs: usize) -> f32 {
    autostart_row_top(n_devs) + ROW_H + SEP_H
}

/// 菜单内 y 坐标的命中结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowHit {
    /// 第 idx 个设备行。
    Device(usize),
    /// 「滚轮模式」勾选行。
    ScrollMode,
    /// 「触发键」录入行。
    ScrollTrigger,
    Autostart,
    Exit,
    /// 标题/滑块/生效规则行/空白等非命令区域。
    Other,
}

/// 按菜单内 y 坐标做行命中测试（全行宽）。
pub fn row_at(y: f32, n_devs: usize) -> RowHit {
    if n_devs > 0 {
        let top = device_row_top(0);
        if y >= top {
            let i = ((y - top) / ROW_H).floor();
            if (i as usize) < n_devs && i >= 0.0 {
                return RowHit::Device(i as usize);
            }
        }
    }
    let mode = scroll_mode_top();
    if y >= mode && y < mode + ROW_H {
        return RowHit::ScrollMode;
    }
    let trig = scroll_trigger_top();
    if y >= trig && y < trig + ROW_H {
        return RowHit::ScrollTrigger;
    }
    let auto = autostart_row_top(n_devs);
    if y >= auto && y < auto + ROW_H {
        return RowHit::Autostart;
    }
    let exit = exit_row_top(n_devs);
    if y >= exit && y < exit + ROW_H {
        return RowHit::Exit;
    }
    RowHit::Other
}

/// 「恢复默认」按钮矩形（标题行右侧，DIP）。
pub const RESET_BTN_W: f32 = 64.0;

pub fn reset_btn_rect() -> (f32, f32, f32, f32) {
    (MENU_W - PAD - RESET_BTN_W, TOP_PAD, MENU_W - PAD, TOP_PAD + TITLE_H)
}

/// 悬停目标（菜单内命中的语义区域）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hover {
    /// 标题行的「恢复默认」按钮。
    Reset,
    /// 设备行。
    Device(usize),
    /// 「滚轮模式」勾选行。
    ScrollMode,
    /// 「触发键」录入行。
    ScrollTrigger,
    Autostart,
    Exit,
}

/// 菜单内 (x, y) 的悬停目标；非命令区域返回 None。
pub fn hover_at(x: f32, y: f32, n_devs: usize) -> Option<Hover> {
    let (l, t, r, b) = reset_btn_rect();
    if x >= l && x < r && y >= t && y < b {
        return Some(Hover::Reset);
    }
    match row_at(y, n_devs) {
        RowHit::Device(i) => Some(Hover::Device(i)),
        RowHit::ScrollMode => Some(Hover::ScrollMode),
        RowHit::ScrollTrigger => Some(Hover::ScrollTrigger),
        RowHit::Autostart => Some(Hover::Autostart),
        RowHit::Exit => Some(Hover::Exit),
        RowHit::Other => None,
    }
}

// ── 菜单动作与交互状态 ─────────────────────────────────

/// 菜单交互动作（由模型交互逻辑产出，宿主层负责执行副作用）。
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    SetSpeed(u32),
    /// 恢复 Windows 默认：指针 10 + 滚轮 3 行/齿。
    ResetDefault,
    /// 以子菜单滑块值保存该设备的规则（指针 + 滚轮）。
    SetRuleWithSpeed(usize, u32, u32),
    ToggleAutostart,
    /// 开/关滚轮模式（按住触发键移动 = 纵向滚轮）。
    ToggleScrollMode,
    /// 触发键录入：已在录入态则取消，否则进入录入（下一个菜单外的鼠标键生效）。
    CaptureTrigger,
    /// 滚轮灵敏度（像素/齿，5–200）。
    SetScrollSens(u32),
    SetRule(usize),
    DelRule(usize),
    Reapply(usize),
    Exit,
}

/// 菜单的全部交互状态（不含绘制）。
pub struct MenuModel {
    pub devs: Vec<DevRow>,
    pub effective: Option<(String, u32)>,
    pub autostart_on: bool,
    /// 已应用的指针速度（1–20）。
    pub speed_val: u32,
    /// 滑块拖动中的预览值；None = 无待提交改动（松手才应用到系统）。
    pub pending_speed: Option<u32>,
    /// 已应用的滚轮速度（1–100 行/齿）。
    pub wheel_val: u32,
    /// 滚轮滑块拖动中的预览值。
    pub pending_wheel: Option<u32>,
    /// 滚轮模式开关（按住触发键移动 = 纵向滚轮）。
    pub scroll_on: bool,
    /// 滚轮模式触发键。
    pub scroll_trigger: TriggerBtn,
    /// 已应用的滚轮灵敏度（像素/齿，5–200）。
    pub scroll_px: u32,
    /// 灵敏度滑块拖动中的预览值。
    pub pending_scroll_px: Option<u32>,
    /// 正在录入触发键（下一个菜单外的鼠标键成为触发键）。
    pub capturing: bool,
    /// 子菜单目标：设备索引 + 行的菜单内 y 起点。Some = 子菜单应显示。
    pub sub_hover: Option<(usize, f32)>,
    /// 子菜单滑块的本地预览值（纯本地，点「设为规则」才写入规则）。
    pub sub_slider: Option<u32>,
    /// 子菜单滚轮滑块的本地预览值。
    pub sub_wheel: Option<u32>,
    /// 子菜单窗口回报的「指针在子菜单内」。
    /// 光标在菜单与子菜单之间移动时靠它保持子菜单不闪关。
    pub sub_pointer_inside: bool,
    /// 键盘焦点行（扁平索引：0 = 滚轮模式，1 = 触发键，2..2+n 为设备行，
    /// 2+n 为自启行，3+n 为退出行）。
    pub kb_focus: Option<usize>,
}

impl MenuModel {
    pub fn new(
        speed_val: u32,
        wheel_val: u32,
        autostart_on: bool,
        scroll_on: bool,
        scroll_trigger: TriggerBtn,
        scroll_px: u32,
    ) -> MenuModel {
        MenuModel {
            devs: Vec::new(),
            effective: None,
            autostart_on,
            speed_val: speed_val.clamp(1, 20),
            pending_speed: None,
            wheel_val: wheel_val.clamp(1, 100),
            pending_wheel: None,
            scroll_on,
            scroll_trigger,
            scroll_px: scroll_px.clamp(SCROLL_PX_MIN, SCROLL_PX_MAX),
            pending_scroll_px: None,
            capturing: false,
            sub_hover: None,
            sub_slider: None,
            sub_wheel: None,
            sub_pointer_inside: false,
            kb_focus: None,
        }
    }

    /// 标题行显示的指针速度（拖动中显示预览值）。
    pub fn display_speed(&self) -> u32 {
        self.pending_speed.unwrap_or(self.speed_val)
    }

    /// 标题行显示的滚轮速度。
    pub fn display_wheel(&self) -> u32 {
        self.pending_wheel.unwrap_or(self.wheel_val)
    }

    /// 滑块拖动中：只更新预览（夹取 1–20），不改已应用值。
    pub fn preview_speed(&mut self, v: i32) {
        self.pending_speed = Some(v.clamp(1, 20) as u32);
    }

    /// 滑块松手：有待提交值且不同于已应用值才返回 Some；提交时同步更新
    /// 模型值（宿主负责写入系统、同步控件与 tooltip）。
    pub fn commit_speed(&mut self) -> Option<u32> {
        let v = self.pending_speed.take()?;
        (v != self.speed_val).then(|| {
            self.speed_val = v;
            v
        })
    }

    /// 滚轮滑块拖动中：只更新预览（夹取 1–100）。
    pub fn preview_wheel(&mut self, v: i32) {
        self.pending_wheel = Some(v.clamp(1, 100) as u32);
    }

    /// 滚轮滑块松手（语义同 [`MenuModel::commit_speed`]）。
    pub fn commit_wheel(&mut self) -> Option<u32> {
        let v = self.pending_wheel.take()?;
        (v != self.wheel_val).then(|| {
            self.wheel_val = v;
            v
        })
    }

    /// 标题行显示的滚轮灵敏度（拖动中显示预览值）。
    pub fn display_scroll_px(&self) -> u32 {
        self.pending_scroll_px.unwrap_or(self.scroll_px)
    }

    /// 灵敏度滑块拖动中：只更新预览（夹取 5–200）。
    pub fn preview_scroll_px(&mut self, v: i32) {
        self.pending_scroll_px = Some(v.clamp(SCROLL_PX_MIN as i32, SCROLL_PX_MAX as i32) as u32);
    }

    /// 灵敏度滑块松手（语义同 [`MenuModel::commit_speed`]）。
    pub fn commit_scroll_px(&mut self) -> Option<u32> {
        let v = self.pending_scroll_px.take()?;
        (v != self.scroll_px).then(|| {
            self.scroll_px = v;
            v
        })
    }

    /// 子菜单滑块预览（夹取 1–20）。
    pub fn preview_sub_slider(&mut self, v: i32) {
        self.sub_slider = Some(v.clamp(1, 20) as u32);
    }

    /// 子菜单滚轮滑块预览（夹取 1–100）。
    pub fn preview_sub_wheel(&mut self, v: i32) {
        self.sub_wheel = Some(v.clamp(1, 100) as u32);
    }

    /// 更新子菜单开合状态：悬停设备行 → 开（记录行位置）；
    /// 指针在子菜单内 → 保持；两者皆无 → 关。
    /// `hovered_dev` 为绘制层命中测试得到的悬停设备行索引。
    pub fn update_hover(&mut self, hovered_dev: Option<usize>) {
        self.sub_hover = match hovered_dev {
            Some(i) => Some((i, device_row_top(i))),
            None if self.sub_pointer_inside => self.sub_hover,
            None => None,
        };
    }

    /// 点击菜单内 (x, y)：映射为命令动作。设备行本身无点击命令
    /// （悬停展开子菜单），标题/滑块/规则行返回空。
    pub fn click_at(&self, x: f32, y: f32) -> Vec<MenuAction> {
        let (l, t, r, b) = reset_btn_rect();
        if x >= l && x < r && y >= t && y < b {
            return vec![MenuAction::ResetDefault];
        }
        match row_at(y, self.devs.len()) {
            RowHit::ScrollMode => vec![MenuAction::ToggleScrollMode],
            RowHit::ScrollTrigger => vec![MenuAction::CaptureTrigger],
            RowHit::Autostart => vec![MenuAction::ToggleAutostart],
            RowHit::Exit => vec![MenuAction::Exit],
            _ => Vec::new(),
        }
    }

    /// 键盘焦点移到下一行（滚轮模式 → 触发键 → 设备行… → 自启 → 退出，循环）。
    pub fn kb_focus_next(&mut self) {
        let n = self.devs.len() + 4;
        self.kb_focus = Some(match self.kb_focus {
            Some(i) => (i + 1) % n,
            None => 0,
        });
    }

    /// 键盘焦点移到上一行。
    pub fn kb_focus_prev(&mut self) {
        let n = self.devs.len() + 4;
        self.kb_focus = Some(match self.kb_focus {
            Some(0) => n - 1,
            Some(i) => i - 1,
            None => n - 1,
        });
    }

    /// Enter/Space 激活当前键盘焦点行。设备行无命令动作。
    /// 焦点扁平索引：0 = 滚轮模式，1 = 触发键，2..2+n = 设备行，2+n = 自启，3+n = 退出。
    pub fn kb_activate(&self) -> Vec<MenuAction> {
        match self.kb_focus {
            Some(0) => vec![MenuAction::ToggleScrollMode],
            Some(1) => vec![MenuAction::CaptureTrigger],
            Some(i) if i >= 2 && i < 2 + self.devs.len() => Vec::new(),
            Some(i) if i == 2 + self.devs.len() => vec![MenuAction::ToggleAutostart],
            _ => vec![MenuAction::Exit],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, vid: Option<&str>, pid: Option<&str>, rule: Option<u32>) -> DevRow {
        DevRow {
            name: name.into(),
            vid: vid.map(Into::into),
            pid: pid.map(Into::into),
            rule_speed: rule,
            rule_wheel: None,
            is_effective: false,
        }
    }

    #[test]
    fn device_row_text() {
        let d = dev("轨迹球", Some("046D"), Some("C52B"), Some(4));
        assert_eq!(d.row_text(), "轨迹球  [046D:C52B]");
        assert!(d.can_rule());

        let blind = dev("盲设备", None, None, None);
        assert_eq!(blind.row_text(), "盲设备  [----:----]");
        assert!(!blind.can_rule());
    }

    #[test]
    fn effective_tip_and_header() {
        assert_eq!(tip_text(7, 3, None), "鼠标灵敏度切换 - 指针: 7 · 滚轮: 3");
        assert_eq!(
            tip_text(4, 9, Some(("轨迹球", 4))),
            "鼠标灵敏度切换 - 指针: 4 · 滚轮: 9 · 生效规则: 轨迹球 (速度 4)"
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
        // 指针滑块与滚轮滑块两段布局都在设备区之上
        assert!(device_row_top(0) > wheel_slider_top() + SLIDER_H);
        assert!((wheel_slider_top() - wheel_label_top() - TITLE_H).abs() < 1e-3);
    }

    #[test]
    fn row_hit_test_bounds() {
        let n = 4;
        // 设备行 0 与行 3
        assert_eq!(row_at(device_row_top(0) + 1.0, n), RowHit::Device(0));
        assert_eq!(row_at(device_row_top(3) + ROW_H - 0.5, n), RowHit::Device(3));
        // 滚轮模式区：勾选行、触发行（顺序在设备区之上）
        assert_eq!(row_at(scroll_mode_top() + 1.0, n), RowHit::ScrollMode);
        assert_eq!(row_at(scroll_trigger_top() + 1.0, n), RowHit::ScrollTrigger);
        assert_eq!(
            row_at(scroll_mode_top() - 1.0, n),
            RowHit::Other,
            "滚轮滑块与模式行之间的分隔带不是命令区"
        );
        // 设备区之后：自启、退出
        assert_eq!(row_at(autostart_row_top(n) + 1.0, n), RowHit::Autostart);
        assert_eq!(row_at(exit_row_top(n) + 1.0, n), RowHit::Exit);
        // 标题、滑块、滚轮行、灵敏度行、规则行、设备区与自启行之间的分隔带、菜单底部 padding
        assert_eq!(row_at(1.0, n), RowHit::Other);
        assert_eq!(row_at(wheel_label_top() + 1.0, n), RowHit::Other);
        assert_eq!(row_at(scroll_sens_label_top() + 1.0, n), RowHit::Other);
        assert_eq!(row_at(eff_row_top() + 1.0, n), RowHit::Other);
        assert_eq!(row_at(device_row_top(n) + SEP_H / 2.0, n), RowHit::Other);
        assert_eq!(row_at(menu_height(n) - 1.0, n), RowHit::Other);
        // 无设备时设备区为占位信息行，不是 Device
        assert_eq!(row_at(device_row_top(0) + 1.0, 0), RowHit::Other);
    }

    #[test]
    fn scroll_rows_layout_order() {
        // 垂直顺序：滚轮滑块 → 模式行 → 触发行 → 灵敏度标签 → 灵敏度滑块 → 规则行
        assert!(scroll_mode_top() >= wheel_slider_top() + SLIDER_H);
        assert!(scroll_trigger_top() >= scroll_mode_top() + ROW_H);
        assert!(scroll_sens_slider_top() >= scroll_sens_label_top() + TITLE_H);
        assert!(eff_row_top() >= scroll_sens_slider_top() + SLIDER_H);
        assert!(device_row_top(0) > eff_row_top());
    }

    fn model() -> MenuModel {
        MenuModel {
            devs: vec![dev("A", Some("1"), Some("2"), None), dev("B", None, None, None)],
            effective: None,
            autostart_on: false,
            speed_val: 10,
            pending_speed: None,
            wheel_val: 3,
            pending_wheel: None,
            scroll_on: false,
            scroll_trigger: TriggerBtn::X1,
            scroll_px: 40,
            pending_scroll_px: None,
            capturing: false,
            sub_hover: None,
            sub_slider: None,
            sub_wheel: None,
            sub_pointer_inside: false,
            kb_focus: None,
        }
    }

    #[test]
    fn hover_state_machine() {
        let mut m = model();
        // 悬停设备行 0 → 子菜单打开，记录行位置
        m.update_hover(Some(0));
        assert_eq!(m.sub_hover, Some((0, device_row_top(0))));

        // 光标进入子菜单（菜单收不到指针）→ 保持
        m.sub_pointer_inside = true;
        m.update_hover(None);
        assert!(m.sub_hover.is_some(), "移入子菜单不应关闭");

        // 光标离开子菜单且不在设备行 → 关闭
        m.sub_pointer_inside = false;
        m.update_hover(None);
        assert!(m.sub_hover.is_none(), "指针离开后应关闭");

        // 悬停非设备行（自启行）→ 无子菜单
        m.update_hover(None);
        assert!(m.sub_hover.is_none());
        // 悬停另一设备行 → 切换目标
        m.update_hover(Some(1));
        assert_eq!(m.sub_hover, Some((1, device_row_top(1))));
    }

    #[test]
    fn click_rows_produce_actions() {
        let n = 2;
        let m = model();
        assert_eq!(
            m.click_at(60.0, scroll_mode_top() + 5.0),
            vec![MenuAction::ToggleScrollMode]
        );
        assert_eq!(
            m.click_at(60.0, scroll_trigger_top() + 5.0),
            vec![MenuAction::CaptureTrigger]
        );
        assert_eq!(m.click_at(60.0, autostart_row_top(n) + 5.0), vec![MenuAction::ToggleAutostart]);
        assert_eq!(m.click_at(60.0, exit_row_top(n) + 5.0), vec![MenuAction::Exit]);
        // 设备行、标题行无点击命令
        assert!(m.click_at(60.0, device_row_top(0) + 5.0).is_empty());
        assert!(m.click_at(60.0, 1.0).is_empty());
    }

    #[test]
    fn hover_and_click_reset_button() {
        let n = 2;
        let m = model();
        // 按钮中心
        let (l, t, r, b) = reset_btn_rect();
        assert_eq!(hover_at((l + r) / 2.0, (t + b) / 2.0, n), Some(Hover::Reset));
        // 按钮点击 → ResetDefault
        assert_eq!(m.click_at((l + r) / 2.0, (t + b) / 2.0), vec![MenuAction::ResetDefault]);
        // 按钮左侧仍是标题行，非命令区
        assert_eq!(hover_at(l - 10.0, (t + b) / 2.0, n), None);
        // 设备行 hover
        assert_eq!(hover_at(60.0, device_row_top(0) + 5.0, n), Some(Hover::Device(0)));
    }

    #[test]
    fn kb_focus_navigation_and_activation() {
        let mut m = model(); // 2 台设备 → 索引 0 滚轮模式；1 触发键；2,3 设备；4 自启；5 退出
        assert_eq!(m.kb_activate(), vec![MenuAction::Exit], "无焦点时 Enter 退出（保底）");
        m.kb_focus_next();
        assert_eq!(m.kb_focus, Some(0));
        assert_eq!(m.kb_activate(), vec![MenuAction::ToggleScrollMode]);
        m.kb_focus_next();
        assert_eq!(m.kb_focus, Some(1));
        assert_eq!(m.kb_activate(), vec![MenuAction::CaptureTrigger]);
        m.kb_focus_next();
        assert_eq!(m.kb_focus, Some(2));
        assert!(m.kb_activate().is_empty(), "设备行 Enter 无动作");
        m.kb_focus_next();
        assert_eq!(m.kb_focus, Some(3));
        assert!(m.kb_activate().is_empty(), "设备行 Enter 无动作");
        m.kb_focus_next();
        assert_eq!(m.kb_activate(), vec![MenuAction::ToggleAutostart]);
        m.kb_focus_next();
        assert_eq!(m.kb_activate(), vec![MenuAction::Exit]);
        // 循环回滚轮模式行
        m.kb_focus_next();
        assert_eq!(m.kb_focus, Some(0));
        // 反向
        m.kb_focus_prev();
        assert_eq!(m.kb_focus, Some(5));
        m.kb_focus_prev();
        m.kb_focus_prev();
        assert_eq!(m.kb_focus, Some(3));
    }

    #[test]
    fn slider_preview_then_commit() {
        let mut m = model();
        // 拖动中：只改预览，已应用值不变
        m.preview_speed(15);
        assert_eq!(m.pending_speed, Some(15));
        assert_eq!(m.speed_val, 10);
        assert_eq!(m.display_speed(), 15);
        // 预览夹取
        m.preview_speed(0);
        assert_eq!(m.pending_speed, Some(1));
        m.preview_speed(99);
        assert_eq!(m.pending_speed, Some(20));
        // 松手：预览值提交为已应用值
        assert_eq!(m.commit_speed(), Some(20));
        assert_eq!(m.speed_val, 20);
        assert_eq!(m.pending_speed, None);
        // 预览值等于已应用值 → 松手不算改动
        m.preview_speed(20);
        assert_eq!(m.commit_speed(), None);
        assert_eq!(m.speed_val, 20);
        // 无拖动直接松手 → None
        assert_eq!(m.commit_speed(), None);
    }

    #[test]
    fn wheel_slider_preview_then_commit() {
        let mut m = model();
        m.preview_wheel(0);
        assert_eq!(m.pending_wheel, Some(1), "下界夹取 1");
        m.preview_wheel(150);
        assert_eq!(m.pending_wheel, Some(100), "上界夹取 100");
        assert_eq!(m.display_wheel(), 100);
        assert_eq!(m.wheel_val, 3);
        assert_eq!(m.commit_wheel(), Some(100));
        assert_eq!(m.wheel_val, 100);
        // 同值不提交；无拖动为 None
        m.preview_wheel(100);
        assert_eq!(m.commit_wheel(), None);
        assert_eq!(m.commit_wheel(), None);
    }

    #[test]
    fn scroll_px_preview_then_commit() {
        let mut m = model();
        m.preview_scroll_px(0);
        assert_eq!(m.pending_scroll_px, Some(5), "下界夹取 5");
        m.preview_scroll_px(999);
        assert_eq!(m.pending_scroll_px, Some(200), "上界夹取 200");
        assert_eq!(m.display_scroll_px(), 200);
        assert_eq!(m.scroll_px, 40);
        assert_eq!(m.commit_scroll_px(), Some(200));
        assert_eq!(m.scroll_px, 200);
        // 同值不提交；无拖动为 None
        m.preview_scroll_px(200);
        assert_eq!(m.commit_scroll_px(), None);
        assert_eq!(m.commit_scroll_px(), None);
    }

    #[test]
    fn sub_slider_preview_clamped() {
        let mut m = model();
        assert_eq!(m.sub_slider, None);
        m.preview_sub_slider(7);
        assert_eq!(m.sub_slider, Some(7));
        m.preview_sub_slider(-3);
        assert_eq!(m.sub_slider, Some(1));
        m.preview_sub_slider(50);
        assert_eq!(m.sub_slider, Some(20));
        // 滚轮预览独立夹取
        m.preview_sub_wheel(0);
        assert_eq!(m.sub_wheel, Some(1));
        m.preview_sub_wheel(150);
        assert_eq!(m.sub_wheel, Some(100));
        m.preview_sub_wheel(9);
        assert_eq!(m.sub_wheel, Some(9));
        assert_eq!(m.sub_slider, Some(20), "滚轮预览不影响指针预览");
    }

    #[test]
    fn sub_layout_hit_tests() {
        // 垂直顺序：信息行 → 指针标签 → 指针滑块 → 滚轮标签 → 滚轮滑块 → 按钮 → 操作行
        assert!(sub_ptr_label_top() > SUB_INFO_TOP);
        assert!(sub_ptr_slider_top() >= sub_ptr_label_top() + SUB_ROW_H);
        assert!(sub_wheel_slider_top() >= sub_wheel_label_top() + SUB_ROW_H);
        assert!(sub_btn_top() >= sub_wheel_slider_top() + SUB_SLIDER_H);
        assert!(sub_action_top(0) >= sub_btn_top() + SUB_ROW_H);
        // 操作行命中：三行各自命中，信息行/标签/滑块/按钮不命中
        assert_eq!(sub_row_at(sub_action_top(0) + 1.0), Some(0));
        assert_eq!(sub_row_at(sub_action_top(2) + SUB_ROW_H - 0.5), Some(2));
        assert_eq!(sub_row_at(SUB_INFO_TOP + 1.0), None, "信息行不是操作行");
        assert_eq!(sub_row_at(sub_ptr_slider_top() + 1.0), None, "滑块行不是操作行");
        assert_eq!(sub_row_at(sub_height() - 1.0), None);
        // 按钮为整行矩形
        let (bl, bt, br, bb) = sub_btn_rect();
        assert!(sub_btn_at((bl + br) / 2.0, (bt + bb) / 2.0));
        assert!(!sub_btn_at((bl + br) / 2.0, bt - 1.0));
        assert!((br - bl - (SUB_W - 2.0 * PAD)).abs() < 0.5, "按钮为整行宽");
        // 指针/滚轮滑块矩形互不重叠且为全宽
        let (sl, _st, sr, sb) = sub_slider_rect();
        let (wl, _wt, wr, _wb) = sub_wheel_rect();
        assert!(sb <= sub_wheel_label_top(), "指针滑块在滚轮标签之上");
        assert!((sr - sl - (SUB_W - 2.0 * PAD)).abs() < 0.5);
        assert!((wr - wl - (SUB_W - 2.0 * PAD)).abs() < 0.5);
    }

    #[test]
    fn dev_row_rule_wheel() {
        let mut m = model();
        m.devs[0].rule_wheel = Some(9);
        assert_eq!(m.devs[0].rule_wheel, Some(9));
        assert_eq!(m.devs[1].rule_wheel, None);
    }
}
