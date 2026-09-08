use std::collections::HashMap;

use crate::config::{Config, Rule};
use crate::devices::{self, Device};
use crate::scroll::ScrollCfg;
use crate::speed;

/// 应用层状态：跟踪当前插入的鼠标、被规则命中的设备在插入前的速度。
pub struct AppState {
    pub cfg: Config,
    /// 当前插入的设备，按插入序（仅用于菜单列表与手动切换）。
    active: Vec<(String, Device)>,
    /// 规则设备的实例 ID -> 该设备激活瞬间的 (指针速度, 滚轮速度)。
    saved: HashMap<String, (u32, u32)>,
    /// 程序内存中最后应用的指针速度。
    applied_speed: u32,
    /// 程序内存中最后应用的滚轮速度。
    applied_wheel: u32,
    /// true = 当前使用全局配置；false = 当前使用 active 末尾的设备特定配置。
    global_active: bool,
}

impl AppState {
    /// 读配置、枚举已插入设备并记录到 active 列表。
    /// 不再在启动时自动应用任何规则，切换完全由用户手动触发。
    pub fn new(mut cfg: Config) -> AppState {
        cfg.speed = cfg.speed.clamp(1, 20);
        cfg.wheel = cfg.wheel.clamp(speed::WHEEL_MIN, speed::WHEEL_MAX);
        let mut s = AppState {
            applied_speed: cfg.speed,
            applied_wheel: cfg.wheel,
            cfg,
            active: Vec::new(),
            saved: HashMap::new(),
            global_active: true,
        };
        speed::set(s.applied_speed);
        speed::set_wheel(s.applied_wheel);
        for dev in devices::enumerate_mice() {
            s.on_device_inserted(&dev);
        }
        s
    }

    fn rule_for(&self, dev: &Device) -> Option<&Rule> {
        let vid = dev.vid.as_deref()?;
        let pid = dev.pid.as_deref()?;
        self.cfg.rule_for(vid, pid)
    }

    /// 设备插入：仅记录到 active 列表，不再自动应用规则。
    fn on_device_inserted(&mut self, dev: &Device) {
        if self.active.iter().any(|(id, _)| id == &dev.instance_id) {
            return;
        }
        self.active.push((dev.instance_id.clone(), dev.clone()));
    }

    /// 设备拔出：仅从 active 列表移除；当前设备配置被拔出时切回全局配置。
    fn on_device_removed(&mut self, instance_id: &str) {
        if !self.active.iter().any(|(id, _)| id == instance_id) {
            return;
        }
        let removed_was_effective = self
            .effective_rule()
            .map(|(d, _)| d.instance_id == instance_id)
            .unwrap_or(false);
        self.active.retain(|(id, _)| id != instance_id);
        self.saved.remove(instance_id);
        if removed_was_effective {
            self.activate_global();
        }
    }

    /// 当前生效的规则设备（active 中最后一条规则设备）及其规则。
    pub fn effective_rule(&self) -> Option<(&Device, &Rule)> {
        if self.global_active {
            return None;
        }
        self.active
            .iter()
            .rev()
            .find_map(|(_, d)| self.rule_for(d).map(|r| (d, r)))
    }

    /// 当前应使用的滚轮模式配置：设备配置期间用规则值（没配 = 关闭），
    /// 全局配置期间使用 `cfg.scroll`。
    /// 返回 None = 滚轮模式整体关闭。
    pub fn effective_scroll(&self) -> Option<ScrollCfg> {
        let s = match self.effective_rule() {
            Some((_, r)) => r.scroll.clone(),
            None => self.cfg.scroll.clone(),
        };
        s.filter(|s| s.enabled)
    }

    /// 更新全局滚轮模式配置的单个字段（主菜单内联滚轮模式区）。
    pub fn update_global_scroll(&mut self, f: impl FnOnce(&mut ScrollCfg)) {
        let mut c = self.cfg.scroll.clone().unwrap_or_default();
        f(&mut c);
        self.cfg.scroll = Some(c);
    }

    pub fn global_active(&self) -> bool {
        self.global_active
    }

    pub fn activate_global(&mut self) {
        self.global_active = true;
        self.applied_speed = self.cfg.speed.clamp(1, 20);
        self.applied_wheel = self.cfg.wheel.clamp(speed::WHEEL_MIN, speed::WHEEL_MAX);
        speed::set(self.applied_speed);
        speed::set_wheel(self.applied_wheel);
    }

    pub fn toggle_rule(&mut self, instance_id: &str) {
        let is_effective = self
            .effective_rule()
            .map(|(d, _)| d.instance_id == instance_id)
            .unwrap_or(false);
        if is_effective {
            self.activate_global();
        } else {
            self.activate_rule(instance_id);
        }
    }

    pub fn set_global_speed(&mut self, v: u32) {
        self.cfg.speed = v.clamp(1, 20);
        if self.global_active {
            self.applied_speed = self.cfg.speed;
            speed::set(self.applied_speed);
        }
    }

    pub fn set_global_wheel(&mut self, v: u32) {
        self.cfg.wheel = v.clamp(speed::WHEEL_MIN, speed::WHEEL_MAX);
        if self.global_active {
            self.applied_wheel = self.cfg.wheel;
            speed::set_wheel(self.applied_wheel);
        }
    }

    pub fn reset_global(&mut self) {
        self.cfg.speed = speed::SPEED_DEFAULT;
        self.cfg.wheel = speed::WHEEL_DEFAULT;
        self.cfg.scroll = None;
        if self.global_active {
            self.activate_global();
        }
    }

    /// 当前插入的设备中命中规则的个数。
    pub fn active_rule_count(&self) -> usize {
        self.active
            .iter()
            .filter(|(_, d)| self.rule_for(d).is_some())
            .count()
    }

    /// 全量重扫：枚举当前设备并应用差异。
    pub fn rescan(&mut self) {
        let now = devices::enumerate_mice();
        self.apply_diff(&now);
    }

    /// 纯逻辑：根据最新的设备列表，处理插入/拔出。
    pub fn apply_diff(&mut self, now: &[Device]) {
        let now_ids: std::collections::HashSet<String> =
            now.iter().map(|d| d.instance_id.clone()).collect();

        let removed: Vec<String> = self
            .active
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| !now_ids.contains(id))
            .collect();
        for id in removed {
            self.on_device_removed(&id);
        }

        for dev in now {
            if !self.active.iter().any(|(id, _)| id == &dev.instance_id) {
                self.on_device_inserted(dev);
            }
        }
    }

    /// 规则被修改/重载后，重新按插入顺序应用所有规则。
    /// 这是一个手动触发的操作（点「设为规则」/「删除规则」后），
    /// 最终结果与 active 中最后一条规则设备相同。
    pub fn reapply(&mut self) {
        let mut cur = speed::get();
        let mut cur_wheel = speed::get_wheel();
        let mut new_saved = HashMap::new();
        let mut applied_rule = false;
        for (id, dev) in &self.active {
            if let Some(rule) = self.rule_for(dev) {
                applied_rule = true;
                new_saved.insert(id.clone(), (cur, cur_wheel));
                cur = rule.speed;
                speed::set(cur);
                if let Some(w) = rule.wheel {
                    cur_wheel = w;
                    speed::set_wheel(w);
                }
            }
        }
        self.saved = new_saved;
        if applied_rule {
            self.applied_speed = cur;
            self.applied_wheel = cur_wheel;
            self.global_active = false;
        } else {
            self.activate_global();
        }
    }

    /// 手动激活某规则设备：从当前位置取出并重新插入到 active 末尾，
    /// 视为「刚刚插入」。记录当前系统速度为该设备的恢复基线，然后应用其规则。
    pub fn activate_rule(&mut self, instance_id: &str) {
        let pos = self.active.iter().position(|(id, _)| id == instance_id);
        let pos = match pos {
            Some(p) => p,
            None => return,
        };

        // 已在末尾且当前正生效：无需改动。
        if pos == self.active.len() - 1 && !self.global_active {
            return;
        }

        let (id, dev) = self.active.remove(pos);

        if let Some(rule) = self.rule_for(&dev) {
            // 先把规则值复制出来，避免借用冲突
            let (target_speed, target_wheel) = (rule.speed, rule.wheel);

            // 记录「激活前」的系统速度作为该设备的恢复基线
            let prev = (speed::get(), speed::get_wheel());
            self.saved.insert(id.clone(), prev);

            self.applied_speed = target_speed;
            speed::set(target_speed);
            if let Some(w) = target_wheel {
                self.applied_wheel = w;
                speed::set_wheel(w);
            }
            self.global_active = false;
        }

        self.active.push((id, dev));
    }

    /// 手动调指针速度（菜单滑块/恢复默认/键盘调速）：写系统并使当前规则失效。
    /// `saved` 恢复基线不受影响——手动重新激活设备时仍按激活时记录的速度恢复。
    pub fn set_speed_manual(&mut self, v: u32) {
        let v = v.clamp(1, 20);
        speed::set(v);
        self.applied_speed = v;
        self.global_active = true;
    }

    /// 手动调滚轮速度：同 `set_speed_manual`，规则一并失效。
    pub fn set_wheel_manual(&mut self, v: u32) {
        let v = v.clamp(speed::WHEEL_MIN, speed::WHEEL_MAX);
        speed::set_wheel(v);
        self.applied_wheel = v;
        self.global_active = true;
    }

    /// WM_SETTINGCHANGE 观察到的系统速度：与本程序最后应用值不同 →
    /// 外部修改（如 Windows 设置），规则失效。本程序自身写入的广播回环
    /// 会因值相等被过滤。返回是否发生了外部修改。
    pub fn on_speed_observed(&mut self, cur_speed: u32, cur_wheel: u32) -> bool {
        if cur_speed == self.applied_speed && cur_wheel == self.applied_wheel {
            return false;
        }
        self.applied_speed = cur_speed;
        self.applied_wheel = cur_wheel;
        self.global_active = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::Device;

    fn rule(vid: &str, pid: &str, speed: u32) -> Rule {
        Rule {
            vid: vid.into(),
            pid: pid.into(),
            speed,
            wheel: None,
            scroll: None,
            note: None,
            alias: None,
        }
    }

    fn rule_with_wheel(vid: &str, pid: &str, speed: u32, wheel: u32) -> Rule {
        Rule {
            wheel: Some(wheel),
            ..rule(vid, pid, speed)
        }
    }

    fn dev(id: &str, vid: &str, pid: &str) -> Device {
        Device {
            instance_id: id.into(),
            vid: Some(vid.into()),
            pid: Some(pid.into()),
            name: id.into(),
        }
    }

    /// 测试会真实调用 SystemParametersInfo 修改系统速度（全局状态）；
    /// 守卫在析构时恢复指针与滚轮，并持有互斥锁保证恢复操作也在串行区间内。
    #[allow(dead_code)]
    struct SpeedGuard(u32, u32, std::sync::MutexGuard<'static, ()>);
    impl Drop for SpeedGuard {
        fn drop(&mut self) {
            speed::set(self.0);
            speed::set_wheel(self.1);
        }
    }

    /// 这些测试真实读写系统级鼠标/滚轮速度（全局状态），并行执行会互相干扰，
    /// 用互斥锁强制串行。
    static SPEED_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 构造一个已配置“轨迹球规则(056E:01C5 -> 4)”的空状态，
    /// 返回 (状态, 原始速度, 原始滚轮, 速度守卫)。
    fn state_with_rule() -> (AppState, u32, u32, SpeedGuard) {
        let lock = SPEED_LOCK.lock().unwrap();
        let original = speed::get();
        let original_wheel = speed::get_wheel();
        let cfg = Config {
            rules: vec![rule("056E", "01C5", 4)],
            speed: original,
            wheel: original_wheel,
            scroll: None,
        };
        let st = AppState {
            cfg,
            active: Vec::new(),
            saved: HashMap::new(),
            applied_speed: original,
            applied_wheel: original_wheel,
            global_active: true,
        };
        (
            st,
            original,
            original_wheel,
            SpeedGuard(original, original_wheel, lock),
        )
    }

    #[test]
    fn insert_only_tracks_device_does_not_apply_rule() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        let tb = dev("ID-A", "056E", "01C5");
        st.on_device_inserted(&tb);
        assert_eq!(st.active.len(), 1);
        // 不再自动应用规则
        assert_eq!(st.applied_speed, original);
        assert_eq!(speed::get(), original);
        assert_eq!(st.applied_wheel, original_wheel);
        assert!(st.saved.is_empty());
        assert!(st.effective_rule().is_none());
    }

    #[test]
    fn removing_effective_device_returns_to_global() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        let tb = dev("ID-A", "056E", "01C5");
        st.apply_diff(&[tb]);
        st.activate_rule("ID-A");
        assert_eq!(speed::get(), 4);

        st.apply_diff(&[]);
        assert_eq!(st.active.len(), 0);
        assert!(st.global_active());
        assert_eq!(speed::get(), original);
        assert_eq!(st.applied_wheel, original_wheel);
    }

    #[test]
    fn activate_rule_applies_and_marks_effective() {
        let (mut st, _original, original_wheel, _g) = state_with_rule();

        st.cfg.rules.push(rule("AAAA", "BBBB", 7));
        let a = dev("ID-A", "056E", "01C5"); // 规则 4
        let b = dev("ID-B", "AAAA", "BBBB"); // 规则 7

        // 仅枚举/记录，不自动切换
        st.apply_diff(&[a, b]);
        assert_eq!(speed::get(), _original);
        assert!(st.effective_rule().is_none());

        // 手动激活 A
        st.activate_rule("ID-A");
        assert_eq!(speed::get(), 4);
        assert_eq!(st.applied_wheel, original_wheel);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-A")
        );
        assert_eq!(st.active.len(), 2);
        assert_eq!(st.active[1].0, "ID-A");

        // 手动激活 B
        st.activate_rule("ID-B");
        assert_eq!(speed::get(), 7);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-B")
        );
    }

    #[test]
    fn manual_speed_overrides_effective_rule() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        st.activate_rule("ID-A");
        assert_eq!(speed::get(), 4);
        assert!(st.effective_rule().is_some());

        // 手动调速 → 规则失效
        st.set_speed_manual(10);
        assert_eq!(speed::get(), 10);
        assert!(st.effective_rule().is_none());

        // 本程序自身写入的广播回环不算外部修改
        assert!(!st.on_speed_observed(10, original_wheel));
        assert!(st.effective_rule().is_none());

        // 手动重新激活：恢复规则速度
        st.activate_rule("ID-A");
        assert_eq!(speed::get(), 4);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-A")
        );

        // 手动调滚轮速度 → 规则失效
        st.set_wheel_manual(20);
        assert_eq!(speed::get_wheel(), 20);
        assert!(st.effective_rule().is_none());

        // 测试结束由 SpeedGuard 恢复系统速度
        assert_eq!(original, original);
    }

    #[test]
    fn apply_diff_tracks_devices_without_switching() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert_eq!(st.active.len(), 1);
        // 不自动切换
        assert_eq!(speed::get(), original);

        st.apply_diff(&[dev("ID-B", "1234", "5678")]);
        assert_eq!(st.active.len(), 1);
        assert_eq!(speed::get(), original);
        assert_eq!(speed::get_wheel(), original_wheel);
    }

    #[test]
    fn reapply_manually_applies_active_rules() {
        let (mut st, _original, _original_wheel, _g) = state_with_rule();

        st.cfg.rules.push(rule("AAAA", "BBBB", 7));
        let a = dev("ID-A", "056E", "01C5");
        let b = dev("ID-B", "AAAA", "BBBB");

        st.apply_diff(&[a, b]);
        assert!(st.effective_rule().is_none());

        // reapply 是手动操作，按 active 顺序应用，最后一条规则生效
        st.reapply();
        assert_eq!(speed::get(), 7);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-B")
        );
    }

    #[test]
    fn rule_wheel_switches_and_restores_on_activate() {
        let (mut st, original, _original_wheel, _g) = state_with_rule();

        st.cfg.rules[0] = rule_with_wheel("056E", "01C5", 4, 9);

        let tb = dev("ID-A", "056E", "01C5");
        st.apply_diff(&[tb]);
        assert_eq!(speed::get(), original);

        st.activate_rule("ID-A");
        assert_eq!(st.applied_speed, 4);
        assert_eq!(speed::get(), 4);
        assert_eq!(st.applied_wheel, 9);
        assert_eq!(speed::get_wheel(), 9);

        st.apply_diff(&[]);
        assert!(st.global_active());
        assert_eq!(speed::get(), original);
        assert_eq!(speed::get_wheel(), st.cfg.wheel);
    }

    #[test]
    fn global_scroll_applies_when_no_rule_effective() {
        use crate::scroll::{ScrollCfg, TriggerBtn};
        let (mut st, _o, _ow, _g) = state_with_rule();

        let global = ScrollCfg {
            enabled: true,
            trigger: TriggerBtn::Middle,
            ..Default::default()
        };
        st.cfg.scroll = Some(global.clone());

        // 无规则生效 → 用全局配置
        assert_eq!(st.effective_scroll(), Some(global.clone()));

        // 规则手动生效但没配滚轮模式 → 全局完全停用（不回落）
        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        st.activate_rule("ID-A");
        assert!(st.effective_rule().is_some());
        assert_eq!(st.effective_scroll(), None);

        // 规则配置了滚轮模式 → 用规则的
        st.cfg.rules[0].scroll = Some(ScrollCfg {
            enabled: true,
            trigger: TriggerBtn::Right,
            ..Default::default()
        });
        assert_eq!(
            st.effective_scroll().map(|s| s.trigger),
            Some(TriggerBtn::Right)
        );

        // 手动调速使规则失效 → 回落到全局配置
        st.set_speed_manual(10);
        assert!(st.effective_rule().is_none());
        assert_eq!(st.effective_scroll(), Some(global));
    }

    #[test]
    fn editing_global_config_does_not_switch_active_device() {
        let (mut st, _o, _ow, _g) = state_with_rule();

        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        st.activate_rule("ID-A");
        assert!(st.effective_rule().is_some());

        st.update_global_scroll(|c| c.enabled = true);
        st.set_global_speed(12);
        st.set_global_wheel(8);
        assert_eq!(speed::get(), 4);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-A")
        );
        assert!(st.effective_scroll().is_none());
    }

    #[test]
    fn active_device_item_toggles_back_to_global() {
        let (mut st, original, original_wheel, _g) = state_with_rule();
        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);

        st.toggle_rule("ID-A");
        assert_eq!(speed::get(), 4);
        assert!(!st.global_active());

        st.toggle_rule("ID-A");
        assert!(st.global_active());
        assert_eq!(speed::get(), original);
        assert_eq!(speed::get_wheel(), original_wheel);
    }

    #[test]
    fn reset_global_clears_scroll_shortcuts() {
        use crate::scroll::{KbTrigger, ScrollCfg, TriggerBtn};
        let (mut st, _o, _ow, _g) = state_with_rule();
        st.cfg.speed = 17;
        st.cfg.wheel = 11;
        st.cfg.scroll = Some(ScrollCfg {
            enabled: true,
            trigger: TriggerBtn::Right,
            kb_trigger: Some(KbTrigger { vk: 0x12 }),
            px_per_line: 80,
        });

        st.reset_global();
        assert_eq!(st.cfg.speed, speed::SPEED_DEFAULT);
        assert_eq!(st.cfg.wheel, speed::WHEEL_DEFAULT);
        assert_eq!(st.cfg.scroll, None);
    }
}
