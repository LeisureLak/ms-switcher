use std::collections::HashMap;

use crate::config::{Config, Rule};
use crate::devices::{self, Device};
use crate::scroll::ScrollCfg;
use crate::speed;

/// 应用层状态：跟踪当前插入的鼠标、被规则命中的设备在插入前的速度。
pub struct AppState {
    pub cfg: Config,
    /// 当前插入的设备，按插入顺序。
    active: Vec<(String, Device)>,
    /// 规则设备的实例 ID -> 该设备插入瞬间的 (指针速度, 滚轮速度)。
    saved: HashMap<String, (u32, u32)>,
    /// 程序内存中最后应用的指针速度。
    applied_speed: u32,
    /// 程序内存中最后应用的滚轮速度。
    applied_wheel: u32,
    /// 手动调速（菜单滑块/恢复默认/键盘 ←/→/外部修改）后置位：
    /// 当前没有任何规则生效，直到下一次规则应用/激活/恢复。
    overridden: bool,
}

impl AppState {
    /// 读配置、枚举已插入设备并立即应用规则。
    pub fn new(cfg: Config) -> AppState {
        let mut s = AppState {
            cfg,
            active: Vec::new(),
            saved: HashMap::new(),
            applied_speed: speed::get(),
            applied_wheel: speed::get_wheel(),
            overridden: false,
        };
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

    /// 设备插入：若命中规则则记录插入前速度并应用规则速度。
    fn on_device_inserted(&mut self, dev: &Device) {
        if self.active.iter().any(|(id, _)| id == &dev.instance_id) {
            return;
        }
        self.active.push((dev.instance_id.clone(), dev.clone()));
        if let Some(rule) = self.rule_for(dev) {
            // 记录真实系统速度（而非内存值），即使外部手动调过滑块也能正确恢复
            let prev = (speed::get(), speed::get_wheel());
            let target_speed = rule.speed;
            let target_wheel = rule.wheel;
            self.saved.insert(dev.instance_id.clone(), prev);
            self.applied_speed = target_speed;
            speed::set(target_speed);
            if let Some(w) = target_wheel {
                self.applied_wheel = w;
                speed::set_wheel(w);
            }
            self.overridden = false;
        }
    }

    /// 设备拔出：若是规则设备，恢复为插入前速度（若有其它规则设备则应用最后插入的规则）。
    fn on_device_removed(&mut self, instance_id: &str) {
        if !self.active.iter().any(|(id, _)| id == instance_id) {
            return;
        }
        self.active.retain(|(id, _)| id != instance_id);
        if let Some((prev_speed, prev_wheel)) = self.saved.remove(instance_id) {
            let last_rule = self
                .active
                .iter()
                .rev()
                .find_map(|(_, d)| self.rule_for(d).map(|r| (r.speed, r.wheel)));
            let (target_speed, target_wheel) = match last_rule {
                // 剩余规则设备：速度必切；滚轮规则未指定则保持现状
                Some((sp, wh)) => (sp, wh),
                None => (prev_speed, Some(prev_wheel)),
            };
            self.applied_speed = target_speed;
            speed::set(target_speed);
            if let Some(w) = target_wheel {
                self.applied_wheel = w;
                speed::set_wheel(w);
            }
            self.overridden = false;
        }
    }

    /// 当前生效的规则设备（active 中最后插入的规则设备）及其规则。
    /// 手动调速或外部改速后（overridden）视为无规则生效。
    pub fn effective_rule(&self) -> Option<(&Device, &Rule)> {
        if self.overridden {
            return None;
        }
        self.active
            .iter()
            .rev()
            .find_map(|(_, d)| self.rule_for(d).map(|r| (d, r)))
    }

    /// 当前应使用的滚轮模式配置：规则生效期间用规则的配置（规则没配 = 关闭），
    /// 无规则生效（含手动调速失效后）回落到全局配置 `cfg.scroll`。
    /// 返回 None = 滚轮模式整体关闭。
    pub fn effective_scroll(&self) -> Option<ScrollCfg> {
        let s = match self.effective_rule() {
            Some((_, r)) => r.scroll.clone(),
            None => self.cfg.scroll.clone(),
        };
        s.filter(|s| s.enabled)
    }

    /// 更新全局滚轮模式配置的单个字段（主菜单内联滚轮模式区）：
    /// 无配置时先建默认配置再改。视为手动调节全局配置 → 当前生效规则失效。
    pub fn update_global_scroll(&mut self, f: impl FnOnce(&mut ScrollCfg)) {
        let mut c = self.cfg.scroll.clone().unwrap_or_default();
        f(&mut c);
        self.cfg.scroll = Some(c);
        self.overridden = true;
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

    /// 规则被修改/重载后，从当前系统速度出发重新按插入顺序应用所有规则。
    pub fn reapply(&mut self) {
        let mut cur = speed::get();
        let mut cur_wheel = speed::get_wheel();
        let mut new_saved = HashMap::new();
        for (id, dev) in &self.active {
            if let Some(rule) = self.rule_for(dev) {
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
        self.applied_speed = cur;
        self.applied_wheel = cur_wheel;
        self.overridden = false;
    }

    /// 手动激活某规则设备：从当前位置取出并重新插入到 active 末尾，
    /// 视为「刚刚插入」。记录当前系统速度为该设备的恢复基线，然后应用其规则。
    pub fn activate_rule(&mut self, instance_id: &str) {
        let pos = self
            .active
            .iter()
            .position(|(id, _)| id == instance_id);
        let pos = match pos {
            Some(p) => p,
            None => return,
        };

        // 已在末尾且未被手动调速覆盖：等价于当前生效，无需改动；
        // overridden 时仍走一遍重新应用，使单击设备行能把规则拉回来
        if pos == self.active.len() - 1 && !self.overridden {
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
            self.overridden = false;
        }

        self.active.push((id, dev));
    }

    /// 手动调指针速度（菜单滑块/恢复默认/键盘调速）：写系统并使当前规则失效。
    /// `saved` 恢复基线不受影响——拔出设备仍按插入/激活时记录的速度恢复。
    pub fn set_speed_manual(&mut self, v: u32) {
        let v = v.clamp(1, 20);
        speed::set(v);
        self.applied_speed = v;
        self.overridden = true;
    }

    /// 手动调滚轮速度：同 `set_speed_manual`，规则一并失效。
    pub fn set_wheel_manual(&mut self, v: u32) {
        let v = v.clamp(speed::WHEEL_MIN, speed::WHEEL_MAX);
        speed::set_wheel(v);
        self.applied_wheel = v;
        self.overridden = true;
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
        self.overridden = true;
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

    /// 构造一个已插入“轨迹球规则(056E:01C5 -> 4)”的空状态，
    /// 返回 (状态, 原始速度, 原始滚轮, 速度守卫)。
    fn state_with_rule() -> (AppState, u32, u32, SpeedGuard) {
        // 必须先持锁再读系统速度：速度是全局真实状态，其它并行测试
        // 的写入都发生在持锁区间内，先读后锁会读到被污染的值。
        let lock = SPEED_LOCK.lock().unwrap();
        let original = speed::get();
        let original_wheel = speed::get_wheel();
        let cfg = Config {
            rules: vec![rule("056E", "01C5", 4)],
            scroll: None,
        };
        let st = AppState {
            cfg,
            active: Vec::new(),
            saved: HashMap::new(),
            applied_speed: original,
            applied_wheel: original_wheel,
            overridden: false,
        };
        (st, original, original_wheel, SpeedGuard(original, original_wheel, lock))
    }

    #[test]
    fn insert_applies_rule_and_remove_restores() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        let tb = dev("ID-A", "056E", "01C5");
        st.on_device_inserted(&tb);
        assert_eq!(st.applied_speed, 4);
        assert_eq!(speed::get(), 4);
        assert_eq!(st.saved.get("ID-A"), Some(&(original, original_wheel)));
        // 规则未指定滚轮 → 滚轮不动
        assert_eq!(st.applied_wheel, original_wheel);
        assert_eq!(speed::get_wheel(), original_wheel);

        st.on_device_removed("ID-A");
        assert_eq!(st.applied_speed, original);
        assert_eq!(speed::get(), original);
        assert_eq!(st.applied_wheel, original_wheel);
        assert_eq!(speed::get_wheel(), original_wheel);
        assert!(!st.saved.contains_key("ID-A"));
    }

    #[test]
    fn rule_wheel_switches_and_restores() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        st.cfg.rules[0] = rule_with_wheel("056E", "01C5", 4, 9);

        let tb = dev("ID-A", "056E", "01C5");
        st.on_device_inserted(&tb);
        assert_eq!(st.applied_speed, 4);
        assert_eq!(speed::get(), 4);
        assert_eq!(st.applied_wheel, 9);
        assert_eq!(speed::get_wheel(), 9);

        st.on_device_removed("ID-A");
        assert_eq!(speed::get(), original);
        assert_eq!(speed::get_wheel(), original_wheel);
    }

    #[test]
    fn non_rule_device_does_not_change_speed() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        let other = dev("ID-B", "1234", "5678");
        st.on_device_inserted(&other);
        assert_eq!(st.applied_speed, original);
        assert_eq!(st.applied_wheel, original_wheel);
        assert!(st.saved.is_empty());
    }

    #[test]
    fn apply_diff_detects_insert_and_remove() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        // 初始：轨迹球已插入
        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert_eq!(st.applied_speed, 4);
        assert_eq!(speed::get(), 4);

        // 拔出轨迹球，插入普通鼠标
        st.apply_diff(&[dev("ID-B", "1234", "5678")]);
        assert_eq!(st.applied_speed, original);
        assert_eq!(speed::get(), original);
        assert_eq!(speed::get_wheel(), original_wheel);
        assert_eq!(st.active.len(), 1);
    }

    #[test]
    fn remove_last_rule_device_restores_original() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        let tb = dev("ID-A", "056E", "01C5");
        let other = dev("ID-B", "1234", "5678");
        // 轨迹球先插，普通鼠标后插（不动速度）
        st.apply_diff(&[tb, other]);
        assert_eq!(st.applied_speed, 4);
        // 拔普通鼠标：轨迹球仍在，速度保持规则值
        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert_eq!(st.applied_speed, 4);
        // 拔轨迹球：无规则设备，恢复插入前速度
        st.apply_diff(&[]);
        assert_eq!(st.applied_speed, original);
        assert_eq!(st.applied_wheel, original_wheel);
    }

    #[test]
    fn two_rule_devices_last_inserted_wins() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        st.cfg.rules.push(rule("AAAA", "BBBB", 7));
        let a = dev("ID-A", "056E", "01C5"); // 规则 4
        let b = dev("ID-B", "AAAA", "BBBB"); // 规则 7

        st.apply_diff(&[a]);
        assert_eq!(st.applied_speed, 4);
        st.apply_diff(&[dev("ID-A", "056E", "01C5"), b]);
        assert_eq!(st.applied_speed, 7);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-B")
        );
        assert_eq!(st.active_rule_count(), 2);
        // 拔出 B：剩余 A 的规则 4 生效
        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert_eq!(st.applied_speed, 4);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-A")
        );
        assert_eq!(st.active_rule_count(), 1);
        // 拔出 A：无规则设备，恢复基线
        st.apply_diff(&[]);
        assert_eq!(st.applied_speed, original);
        assert_eq!(st.applied_wheel, original_wheel);
        assert!(st.effective_rule().is_none());
        assert_eq!(st.active_rule_count(), 0);
    }

    #[test]
    fn activate_rule_moves_to_end_and_changes_effective() {
        let (mut st, _original, _original_wheel, _g) = state_with_rule();

        st.cfg.rules.push(rule("AAAA", "BBBB", 7));
        let a = dev("ID-A", "056E", "01C5"); // 规则 4
        let b = dev("ID-B", "AAAA", "BBBB"); // 规则 7

        // A 先插入，B 后插入：当前生效 B（速度 7）
        st.apply_diff(&[a, b]);
        assert_eq!(st.applied_speed, 7);
        assert_eq!(st.effective_rule().map(|(d, _)| d.instance_id.as_str()), Some("ID-B"));

        // 手动激活 A：A 移到末尾，应用 A 的规则 4
        st.activate_rule("ID-A");
        assert_eq!(st.applied_speed, 4);
        assert_eq!(speed::get(), 4);
        assert_eq!(st.effective_rule().map(|(d, _)| d.instance_id.as_str()), Some("ID-A"));
        assert_eq!(st.active.len(), 2);
        assert_eq!(st.active[1].0, "ID-A");

        // 手动激活已经处于末尾的 B：无变化
        st.activate_rule("ID-B");
        assert_eq!(st.applied_speed, 7);
        assert_eq!(st.effective_rule().map(|(d, _)| d.instance_id.as_str()), Some("ID-B"));
    }

    #[test]
    fn activate_rule_updates_removal_baseline() {
        let (mut st, _original, original_wheel, _g) = state_with_rule();

        st.cfg.rules.push(rule("AAAA", "BBBB", 7));
        let a = dev("ID-A", "056E", "01C5"); // 规则 4
        let b = dev("ID-B", "AAAA", "BBBB"); // 规则 7

        st.apply_diff(&[a, b]);
        // 当前生效 B（速度 7）
        st.activate_rule("ID-A");
        // A 的恢复基线应记录为 B 的规则速度 7
        assert_eq!(st.saved.get("ID-A"), Some(&(7, original_wheel)));

        // 此时 active: [B, A]，拔出 B：剩余 A 生效，速度保持 4
        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert_eq!(st.applied_speed, 4);
        assert_eq!(st.effective_rule().map(|(d, _)| d.instance_id.as_str()), Some("ID-A"));

        // 再拔出 A：无规则设备，应恢复到 A 手动激活前的速度 7
        st.apply_diff(&[]);
        assert_eq!(st.applied_speed, 7);
        assert_eq!(st.applied_wheel, original_wheel);
    }

    #[test]
    fn manual_speed_change_invalidates_effective_rule() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert_eq!(st.applied_speed, 4);
        assert!(st.effective_rule().is_some());

        // 手动调指针速度 → 规则失效；saved 基线不受影响
        st.set_speed_manual(10);
        assert_eq!(speed::get(), 10);
        assert!(st.effective_rule().is_none());
        assert_eq!(st.saved.get("ID-A"), Some(&(original, original_wheel)));

        // 本程序自身写入的广播回环（观察值 == 已应用值）不算外部修改
        assert!(!st.on_speed_observed(10, original_wheel));
        assert!(st.effective_rule().is_none());

        // 单击已在 active 末尾的设备行：overridden 时仍重新应用规则
        st.activate_rule("ID-A");
        assert_eq!(speed::get(), 4);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-A")
        );

        // 手动调滚轮速度 → 规则同样失效
        st.set_wheel_manual(20);
        assert_eq!(speed::get_wheel(), 20);
        assert!(st.effective_rule().is_none());
    }

    #[test]
    fn external_speed_change_invalidates_and_removal_still_restores() {
        let (mut st, original, original_wheel, _g) = state_with_rule();

        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert!(st.effective_rule().is_some());

        // 外部修改（如 Windows 设置）→ 规则失效
        speed::set(12);
        assert!(st.on_speed_observed(speed::get(), speed::get_wheel()));
        assert!(st.effective_rule().is_none());

        // 失效后新插入命中规则的设备 → 重新应用规则，恢复生效
        st.apply_diff(&[dev("ID-A", "056E", "01C5"), dev("ID-B", "056E", "01C5")]);
        assert_eq!(speed::get(), 4);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-B")
        );

        // 失效后拔出 B：回退到剩余规则设备 A（恢复逻辑不变）
        st.set_speed_manual(9);
        assert!(st.effective_rule().is_none());
        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert_eq!(speed::get(), 4);
        assert_eq!(
            st.effective_rule().map(|(d, _)| d.instance_id.as_str()),
            Some("ID-A")
        );
        // 全部拔出：恢复 A 插入时记录的原速
        st.apply_diff(&[]);
        assert_eq!(speed::get(), original);
        assert_eq!(st.applied_wheel, original_wheel);
    }

    #[test]
    fn global_scroll_applies_only_when_no_rule_effective() {
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

        // 规则生效但没配滚轮模式 → 全局完全停用（不回落）
        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
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
    fn update_global_scroll_invalidates_rule() {
        let (mut st, _o, _ow, _g) = state_with_rule();

        st.apply_diff(&[dev("ID-A", "056E", "01C5")]);
        assert!(st.effective_rule().is_some());

        // 修改全局滚轮模式配置 = 手动调节全局配置 → 规则失效
        st.update_global_scroll(|c| c.enabled = true);
        assert!(st.effective_rule().is_none());
        assert!(st.effective_scroll().is_some());
    }
}
