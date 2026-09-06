//! 滚轮模式纯逻辑：触发键枚举与「像素位移 → 滚轮齿」累积器。
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

/// 滚轮灵敏度取值范围与默认值（像素/齿：轨迹球滚动多少像素 = 一齿滚轮）。
/// 值越小越灵敏。Win32 滚轮一齿 = WHEEL_DELTA(120)，在钩子层换算。
pub const SCROLL_PX_MIN: u32 = 5;
pub const SCROLL_PX_MAX: u32 = 200;
pub const SCROLL_PX_DEFAULT: u32 = 40;

/// 纵向滚轮注入量累积器。
///
/// 轨迹球位移精度远高于滚轮齿，按像素累积、攒够一齿注入一次；
/// 小数部分保留，慢滚也能在若干次移动后凑出一齿。
#[derive(Debug, Default, Clone)]
pub struct WheelAccum {
    /// 已累积、尚未换算的像素（向上滚动为正，与滚轮方向一致）。
    accum_px: f32,
}

impl WheelAccum {
    /// 喂入一段位移。`dy_px` 为屏幕坐标 Y 增量（向下为正），
    /// 返回应注入的滚轮量（120 的整数倍，方向：上滚为正）。
    pub fn feed(&mut self, dy_px: f32, px_per_notch: f32) -> i32 {
        if !(px_per_notch > 0.0) {
            return 0;
        }
        // 屏幕坐标向下为正；滚轮向上为正 → 取反
        self.accum_px += -dy_px;
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
#[derive(Debug, Default)]
pub struct ScrollEngine {
    /// 是否处于「按住触发键」的滚轮模式中。
    pub active: bool,
    /// 像素 → 齿累积器。
    pub accum: WheelAccum,
    /// 上一个 WM_MOUSEMOVE 的屏幕 Y（求位移用）。
    pub last_y: i32,
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
    fn accumulates_subpixel_and_emits_notches() {
        let mut a = WheelAccum::default();
        // 40 像素/齿：每次上滚 15 像素，前两次都不够一齿
        assert_eq!(a.feed(-15.0, 40.0), 0);
        assert_eq!(a.feed(-15.0, 40.0), 0);
        // 第三次累计 45 像素 → 一齿，剩 5 像素小数
        assert_eq!(a.feed(-15.0, 40.0), 120);
        // 再两次累计 20、35，都不够一齿
        assert_eq!(a.feed(-15.0, 40.0), 0);
        assert_eq!(a.feed(-15.0, 40.0), 0);
        // 50 像素 → 一齿，剩 10
        assert_eq!(a.feed(-15.0, 40.0), 120);
    }

    #[test]
    fn direction_down_is_negative() {
        let mut a = WheelAccum::default();
        // 向下移动 80 像素 → 2 齿向下滚
        assert_eq!(a.feed(80.0, 40.0), -240);
    }

    #[test]
    fn large_move_emits_multiple_notches() {
        let mut a = WheelAccum::default();
        assert_eq!(a.feed(-190.0, 40.0), 480); // 4.75 齿 → 4 齿，剩 0.75
        assert_eq!(a.feed(-10.0, 40.0), 120); // 剩量凑满
        assert_eq!(a.feed(-0.0, 40.0), 0);
    }

    #[test]
    fn reset_clears_pending() {
        let mut a = WheelAccum::default();
        a.feed(-30.0, 40.0);
        a.reset();
        assert_eq!(a.feed(-5.0, 40.0), 0);
    }

    #[test]
    fn invalid_px_per_notch_is_ignored() {
        let mut a = WheelAccum::default();
        assert_eq!(a.feed(-100.0, 0.0), 0);
        assert_eq!(a.feed(-100.0, -5.0), 0);
    }
}
