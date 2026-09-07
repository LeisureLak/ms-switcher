//! 滚轮模式纯逻辑：触发键枚举与「像素位移 → 滚动行数」累积器。
//!
//! 不依赖任何 OS 类型，可脱离窗口单测；win32/scroll_hook.rs 只做
//! 钩子事件 → 本模块调用的映射与 SendInput 副作用。

use serde::{Deserialize, Serialize};

/// 可作为滚轮模式触发键的鼠标按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerBtn {
    Left,
    Middle,
    Right,
    X1,
    X2,
}

impl Default for TriggerBtn {
    fn default() -> Self {
        TriggerBtn::X1
    }
}

impl TriggerBtn {
    /// 菜单显示名。
    pub fn label(self) -> &'static str {
        match self {
            TriggerBtn::Left => "鼠标左键",
            TriggerBtn::Middle => "鼠标中键",
            TriggerBtn::Right => "鼠标右键",
            TriggerBtn::X1 => "鼠标侧键1",
            TriggerBtn::X2 => "鼠标侧键2",
        }
    }

    /// 稳定的数值编码（跨消息传递/存静态量用）。
    pub fn code(self) -> u32 {
        match self {
            TriggerBtn::Left => 1,
            TriggerBtn::Middle => 2,
            TriggerBtn::Right => 3,
            TriggerBtn::X1 => 4,
            TriggerBtn::X2 => 5,
        }
    }

    pub fn from_code(c: u32) -> Option<TriggerBtn> {
        Some(match c {
            1 => TriggerBtn::Left,
            2 => TriggerBtn::Middle,
            3 => TriggerBtn::Right,
            4 => TriggerBtn::X1,
            5 => TriggerBtn::X2,
            _ => return None,
        })
    }
}

/// 可作为滚轮模式开关键的键盘单键（支持左右 Shift/Ctrl/Alt/Win 归一化）。
///
/// 语义：单独按住该键时进入滚轮模式；按住期间若又按下其它任意键，
/// 立即取消滚轮模式，避免影响 `Alt+Tab`、`Ctrl+C` 等系统/应用组合键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbTrigger {
    /// 归一化后的虚拟键码。
    pub vk: u32,
}

impl KbTrigger {
    /// 显示名（用于菜单与 tooltip）。
    pub fn label(self) -> String {
        vk_label(self.vk_canonical())
    }

    /// 将左右 Shift/Ctrl/Alt/Win 映射到同一语义键，便于匹配。
    pub fn vk_canonical(self) -> u32 {
        canonicalize_vk(self.vk)
    }
}

/// 把左右成对的修饰键归一化到同一个虚拟键码。
fn canonicalize_vk(vk: u32) -> u32 {
    match vk {
        0xA0 | 0xA1 => 0x10, // VK_SHIFT
        0xA2 | 0xA3 => 0x11, // VK_CONTROL
        0xA4 | 0xA5 => 0x12, // VK_MENU (Alt)
        0x5C => 0x5B,        // VK_RWIN -> VK_LWIN
        _ => vk,
    }
}

/// 归一化虚拟键码对应的显示名。
fn vk_label(vk: u32) -> String {
    match vk {
        0x08 => "Backspace".to_string(),
        0x09 => "Tab".to_string(),
        0x0D => "Enter".to_string(),
        0x1B => "Esc".to_string(),
        0x20 => "Space".to_string(),
        0x21 => "PgUp".to_string(),
        0x22 => "PgDn".to_string(),
        0x23 => "End".to_string(),
        0x24 => "Home".to_string(),
        0x25 => "Left".to_string(),
        0x26 => "Up".to_string(),
        0x27 => "Right".to_string(),
        0x28 => "Down".to_string(),
        0x2C => "PrtSc".to_string(),
        0x2D => "Insert".to_string(),
        0x2E => "Delete".to_string(),
        0x30..=0x39 => format!("{}", (b'0' + (vk - 0x30) as u8) as char),
        0x41..=0x5A => format!("{}", (vk as u8) as char),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x90 => "Num Lock".to_string(),
        0x91 => "Scroll Lock".to_string(),
        0xA0 => "LShift".to_string(),
        0xA1 => "RShift".to_string(),
        0xA2 => "LCtrl".to_string(),
        0xA3 => "RCtrl".to_string(),
        0xA4 => "LAlt".to_string(),
        0xA5 => "RAlt".to_string(),
        0x5B => "Win".to_string(),
        0x5C => "Win".to_string(),
        0x10 => "Shift".to_string(),
        0x11 => "Ctrl".to_string(),
        0x12 => "Alt".to_string(),
        0x14 => "Caps Lock".to_string(),
        _ => format!("VK 0x{vk:02X}"),
    }
}

/// 滚轮灵敏度取值范围与默认值（像素/行：轨迹球滚动多少像素 = 滚动一行）。
/// 值越小越灵敏。注入以整齿为最小单位：一齿 = 像素/行 × 系统「每齿行数」，
/// 每步滚动精确等于系统行/齿（见 win32/scroll_hook.rs）。
pub const SCROLL_PX_MIN: u32 = 2;
pub const SCROLL_PX_MAX: u32 = 200;
pub const SCROLL_PX_DEFAULT: u32 = 15;

/// 设备的滚轮模式配置（按规则保存）：开/关 + 鼠标触发键 + 键盘触发键 + 灵敏度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrollCfg {
    pub enabled: bool,
    pub trigger: TriggerBtn,
    /// 可选的键盘开关键（单键，组合键触发时取消）。
    pub kb_trigger: Option<KbTrigger>,
    /// 灵敏度（像素/行）。兼容读取旧字段 `px_per_notch`（像素/齿）：
    /// 旧值按新语义直接采用，不做数值换算。
    #[serde(alias = "px_per_notch")]
    pub px_per_line: u32,
}

impl Default for ScrollCfg {
    fn default() -> Self {
        ScrollCfg {
            enabled: false,
            trigger: TriggerBtn::X1,
            kb_trigger: None,
            px_per_line: SCROLL_PX_DEFAULT,
        }
    }
}

/// 纵向滚轮注入量累积器。
///
/// 轨迹球位移按像素累积，攒够「一齿」（= 像素/行 × 系统行每齿）才注入
/// 一次整齿 delta（120 的倍数）——与真实滚轮完全一致：每步滚动精确等于
/// 系统「每齿行数」。不足一齿的像素留在累积器里，慢滚也能凑出整齿。
#[derive(Debug, Default, Clone)]
pub struct WheelAccum {
    /// 已累积、尚未换算的像素（向上滚动为正，与滚轮方向一致）。
    accum_px: f32,
}

impl WheelAccum {
    /// 喂入一段位移。`dy_px` 为纵向位移（向下为正；Raw Input 相对位移
    /// 与屏幕坐标同向，灵敏度以像素/行标定，量级一致），
    /// `lines_per_notch` 为系统「每齿行数」。
    /// 返回应注入的滚轮量（120 的整数倍，方向：上滚为正）。
    pub fn feed(&mut self, dy_px: f32, px_per_line: f32, lines_per_notch: f32) -> i32 {
        if !(px_per_line > 0.0) || !(lines_per_notch > 0.0) {
            return 0;
        }
        // 屏幕坐标向下为正；滚轮向上为正 → 取反
        self.accum_px += -dy_px;
        // 一齿 = 像素/行 × 行每齿；只发整齿 delta，保证每步滚动
        // 精确等于系统「每齿行数」（亚齿 delta 依赖应用自行累积，
        // 各应用实现不一，对不齐系统行/齿）
        let px_per_notch = px_per_line * lines_per_notch;
        let notches = (self.accum_px / px_per_notch).trunc();
        if notches == 0.0 {
            return 0;
        }
        self.accum_px -= notches * px_per_notch;
        // 滚轮单次 delta 是 i16：限幅防极端值
        let notches = notches.clamp(-(32767 / 120) as f32, (32767 / 120) as f32);
        notches as i32 * 120
    }

    /// 累积器清零（进入滚轮模式 / 切换灵敏度时调用）。
    pub fn reset(&mut self) {
        self.accum_px = 0.0;
    }
}

/// 钩子侧的滚轮模式运行时状态（由宿主单线程持有，钩子回调同线程借用）。
#[derive(Debug)]
pub struct ScrollEngine {
    /// 当前生效规则是否启用了滚轮模式。
    pub enabled: bool,
    /// 是否处于「按住触发键」的滚轮模式中。
    pub active: bool,
    /// 鼠标触发键当前是否被按住。
    pub mouse_held: bool,
    /// 键盘开关键当前是否被单独按住（无其它非注入键）。
    pub kb_held: bool,
    /// 位移 → 齿累积器（位移源是 Raw Input 相对位移，见 win32/scroll_hook.rs）。
    pub accum: WheelAccum,
    /// 当前生效规则的鼠标触发键。
    pub trigger: TriggerBtn,
    /// 当前生效规则的键盘开关键（单键）。
    pub kb_trigger: Option<KbTrigger>,
    /// 当前生效规则的灵敏度（像素/行）。
    pub px_per_line: u32,
}

impl Default for ScrollEngine {
    fn default() -> Self {
        ScrollEngine {
            enabled: false,
            active: false,
            mouse_held: false,
            kb_held: false,
            accum: WheelAccum::default(),
            trigger: TriggerBtn::X1,
            kb_trigger: None,
            px_per_line: SCROLL_PX_DEFAULT,
        }
    }
}

impl ScrollEngine {
    /// 根据鼠标/键盘两个来源的当前状态重新计算 `active`。
    /// `kb_held` 要求调用方已保证「键盘开关键被单独按住（无其它键）」。
    /// 进入滚轮模式时清零累积器，退出时不动（待注入量仍由队列冲刷）。
    pub fn sync_active(&mut self) {
        if !self.enabled {
            self.active = false;
            return;
        }
        let want = self.mouse_held || self.kb_held;
        if want && !self.active {
            self.accum.reset();
        }
        self.active = want;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_btn_roundtrip_and_labels() {
        for c in 1..=5u32 {
            let t = TriggerBtn::from_code(c).unwrap();
            assert_eq!(t.code(), c);
            assert!(!t.label().is_empty());
        }
        assert_eq!(TriggerBtn::from_code(0), None);
        assert_eq!(TriggerBtn::from_code(6), None);
        // serde 字符串形式（配置持久化格式）
        assert_eq!(serde_json::to_string(&TriggerBtn::X1).unwrap(), "\"x1\"");
        assert_eq!(
            serde_json::from_str::<TriggerBtn>("\"middle\"").unwrap(),
            TriggerBtn::Middle
        );
    }

    #[test]
    fn default_trigger_is_x1() {
        assert_eq!(TriggerBtn::default(), TriggerBtn::X1);
    }

    #[test]
    fn accumulates_subnotch_and_emits_whole_notches() {
        let mut a = WheelAccum::default();
        // 10 像素/行、系统 4 行/齿 → 一齿 40 像素；只发整齿 delta
        assert_eq!(a.feed(-15.0, 10.0, 4.0), 0);
        assert_eq!(a.feed(-15.0, 10.0, 4.0), 0);
        // 第三次累计 45 像素 → 一齿，剩 5 像素
        assert_eq!(a.feed(-15.0, 10.0, 4.0), 120);
        assert_eq!(a.feed(-15.0, 10.0, 4.0), 0);
        assert_eq!(a.feed(-15.0, 10.0, 4.0), 0);
        // 50 像素 → 一齿，剩 10
        assert_eq!(a.feed(-15.0, 10.0, 4.0), 120);
    }

    #[test]
    fn direction_down_is_negative() {
        let mut a = WheelAccum::default();
        // 80 像素、每齿 40 像素（10 像素/行 × 4 行）→ 2 齿向下滚
        assert_eq!(a.feed(80.0, 10.0, 4.0), -240);
    }

    #[test]
    fn lines_per_notch_scales_pixels_per_step() {
        // 同一灵敏度下，行/齿越大每步需要的像素越多、每步滚动的行数也越多
        let mut a = WheelAccum::default();
        assert_eq!(a.feed(-15.0, 15.0, 1.0), 120); // 1 行/齿 → 15px 即一齿
        let mut b = WheelAccum::default();
        assert_eq!(b.feed(-15.0, 15.0, 3.0), 0); // 3 行/齿 → 需 45px
        assert_eq!(b.feed(-45.0, 15.0, 3.0), 120); // 累计 60 → 一齿，剩 15
    }

    #[test]
    fn large_move_emits_multiple_notches() {
        let mut a = WheelAccum::default();
        assert_eq!(a.feed(-190.0, 10.0, 4.0), 480); // 190/40 → 4 齿，剩 30 像素
        assert_eq!(a.feed(-10.0, 10.0, 4.0), 120); // 剩量凑满一齿
        assert_eq!(a.feed(-1.0, 10.0, 4.0), 0);
    }

    #[test]
    fn reset_clears_pending() {
        let mut a = WheelAccum::default();
        a.feed(-10.0, 15.0, 3.0);
        a.reset();
        assert_eq!(a.feed(-5.0, 15.0, 3.0), 0);
    }

    #[test]
    fn invalid_params_are_ignored() {
        let mut a = WheelAccum::default();
        assert_eq!(a.feed(-100.0, 0.0, 3.0), 0);
        assert_eq!(a.feed(-100.0, -5.0, 3.0), 0);
        assert_eq!(a.feed(-100.0, 15.0, 0.0), 0);
    }

    #[test]
    fn kb_trigger_canonicalizes_sides() {
        let raw = KbTrigger { vk: 0xA4 }; // Left Alt
        assert_eq!(raw.vk_canonical(), 0x12);
        let right = KbTrigger { vk: 0xA5 };
        assert_eq!(right.vk_canonical(), 0x12);
        assert_eq!(right.label(), "Alt");
        assert_eq!(KbTrigger { vk: 0xA0 }.label(), "Shift");
        assert_eq!(KbTrigger { vk: 0x5C }.label(), "Win");
        assert_eq!(KbTrigger { vk: 0x70 }.label(), "F1");
    }

    #[test]
    fn scroll_cfg_with_kb_trigger_serde_roundtrip() {
        let cfg = ScrollCfg {
            enabled: true,
            trigger: TriggerBtn::X2,
            kb_trigger: Some(KbTrigger { vk: 0xA4 }),
            px_per_line: 30,
        };
        let s = serde_json::to_string(&cfg).unwrap();
        assert!(s.contains("kb_trigger"));
        let back: ScrollCfg = serde_json::from_str(&s).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn scroll_cfg_legacy_px_per_notch_alias() {
        // 旧配置字段 px_per_notch（像素/齿）按新语义直接读为像素/行；
        // 缺失字段走容器默认
        let legacy: ScrollCfg = serde_json::from_str(
            r#"{"enabled":true,"trigger":"x2","px_per_notch":30}"#,
        )
        .unwrap();
        assert_eq!(legacy.px_per_line, 30);
        assert_eq!(legacy.kb_trigger, None);
    }

    #[test]
    fn scroll_engine_sync_active_handles_both_sources() {
        let mut e = ScrollEngine {
            enabled: true,
            active: false,
            mouse_held: false,
            kb_held: false,
            accum: WheelAccum::default(),
            trigger: TriggerBtn::X1,
            kb_trigger: Some(KbTrigger { vk: 0xA4 }),
            px_per_line: SCROLL_PX_DEFAULT,
        };
        // 鼠标按住 → 激活
        e.mouse_held = true;
        e.sync_active();
        assert!(e.active);
        e.mouse_held = false;
        e.sync_active();
        assert!(!e.active);
        // 键盘按住（调用方已保证单独） → 激活
        e.kb_held = true;
        e.sync_active();
        assert!(e.active);
        // 未启用则强制关闭
        e.enabled = false;
        e.sync_active();
        assert!(!e.active);
    }
}
