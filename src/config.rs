use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

/// 一条灵敏度切换规则：当 VID:PID 匹配的设备插入时，把指针速度设为 `speed`，
/// 若指定了 `wheel` 也把滚轮速度（行/齿）设为该值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub vid: String,
    #[serde(default)]
    pub pid: String,
    #[serde(default = "default_speed")]
    pub speed: u32,
    /// 插入时应用的滚轮速度（1-100 行/齿）；None = 规则不改滚轮。
    #[serde(default)]
    pub wheel: Option<u32>,
    #[serde(default)]
    pub note: Option<String>,
}

fn default_speed() -> u32 {
    10
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl Config {
    pub fn rule_for(&self, vid: &str, pid: &str) -> Option<&Rule> {
        self.rules
            .iter()
            .find(|r| r.vid.eq_ignore_ascii_case(vid) && r.pid.eq_ignore_ascii_case(pid))
    }

    /// 设置（或更新）一条规则；返回该规则最终的速度。
    pub fn set_rule(
        &mut self,
        vid: &str,
        pid: &str,
        speed: u32,
        wheel: Option<u32>,
        note: Option<String>,
    ) {
        if let Some(r) = self
            .rules
            .iter_mut()
            .find(|r| r.vid.eq_ignore_ascii_case(vid) && r.pid.eq_ignore_ascii_case(pid))
        {
            r.speed = speed;
            r.wheel = wheel;
            r.note = note;
        } else {
            self.rules.push(Rule {
                vid: vid.to_ascii_uppercase(),
                pid: pid.to_ascii_uppercase(),
                speed,
                wheel,
                note,
            });
        }
    }

    pub fn remove_rule(&mut self, vid: &str, pid: &str) -> bool {
        let before = self.rules.len();
        self.rules.retain(|r| {
            !(r.vid.eq_ignore_ascii_case(vid) && r.pid.eq_ignore_ascii_case(pid))
        });
        self.rules.len() != before
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("MouseSpeedSwitcher").join("config.json")
}

pub fn load() -> Config {
    match fs::read_to_string(config_path()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> io::Result<()> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(cfg).unwrap();
    fs::write(&p, text)
}
