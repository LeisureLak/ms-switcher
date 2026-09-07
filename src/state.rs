use std::collections::HashMap;

use crate::config::{Config, Rule};
use crate::devices::{self, Device};
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
        }
    }

    /// 当前生效的规则设备（active 中最后插入的规则设备）及其规则。
    pub fn effective_rule(&self) -> Option<(&Device, &Rule)> {
        self.active
            .iter()
            .rev()
            .find_map(|(_, d)| self.rule_for(d).map(|r| (d, r)))
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
    /// 守卫保证断言失败时也恢复指针与滚轮。
    struct SpeedGuard(u32, u32);
    impl Drop for SpeedGuard {
        fn drop(&mut self) {
            speed::set(self.0);
            speed::set_wheel(self.1);
        }
    }

    /// 这些测试真实读写系统级鼠标/滚轮速度（全局状态），并行执行会互相干扰，
    /// 用互斥锁强制串行。
    static SPEED_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 构造一个已插入“轨迹球规则(056E:01C5 -> 4)”的空状态，返回 (状态, 原始速度, 原始滚轮)。
    fn state_with_rule() -> (AppState, u32, u32) {
        // 必须先持锁再读系统速度：速度是全局真实状态，其它并行测试
        // 的写入都发生在持锁区间内，先读后锁会读到被污染的值。
        let _lock = SPEED_LOCK.lock().unwrap();
        let original = speed::get();
        let original_wheel = speed::get_wheel();
        let cfg = Config {
            rules: vec![rule("056E", "01C5", 4)],
        };
        let st = AppState {
            cfg,
            active: Vec::new(),
            saved: HashMap::new(),
            applied_speed: original,
            applied_wheel: original_wheel,
        };
        (st, original, original_wheel)
    }

    #[test]
    fn insert_applies_rule_and_remove_restores() {
        let (mut st, original, original_wheel) = state_with_rule();
        let _g = SpeedGuard(original, original_wheel);
        let _lock = SPEED_LOCK.lock().unwrap();
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
        let (mut st, original, original_wheel) = state_with_rule();
        let _g = SpeedGuard(original, original_wheel);
        let _lock = SPEED_LOCK.lock().unwrap();
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
        let (mut st, original, original_wheel) = state_with_rule();
        let _g = SpeedGuard(original, original_wheel);
        let _lock = SPEED_LOCK.lock().unwrap();
        let other = dev("ID-B", "1234", "5678");
        st.on_device_inserted(&other);
        assert_eq!(st.applied_speed, original);
        assert_eq!(st.applied_wheel, original_wheel);
        assert!(st.saved.is_empty());
    }

    #[test]
    fn apply_diff_detects_insert_and_remove() {
        let (mut st, original, original_wheel) = state_with_rule();
        let _g = SpeedGuard(original, original_wheel);
        let _lock = SPEED_LOCK.lock().unwrap();
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
        let (mut st, original, original_wheel) = state_with_rule();
        let _g = SpeedGuard(original, original_wheel);
        let _lock = SPEED_LOCK.lock().unwrap();
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
        let (mut st, original, original_wheel) = state_with_rule();
        let _g = SpeedGuard(original, original_wheel);
        let _lock = SPEED_LOCK.lock().unwrap();
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
}
