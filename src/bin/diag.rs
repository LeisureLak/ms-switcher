use mouse_speed_switcher::{config, devices, speed};

/// 诊断工具：打印当前指针速度、配置与所有鼠标设备（用于验证设备识别）。
fn main() {
    println!("当前指针速度: {}", speed::get());
    println!("配置文件: {}", config::config_path().display());

    let cfg = config::load();
    println!("已配置规则: {}", cfg.rules.len());
    for r in &cfg.rules {
        println!(
            "  VID:{} PID:{} -> 速度 {} {}",
            r.vid,
            r.pid,
            r.speed,
            r.note.as_deref().unwrap_or("")
        );
    }

    println!("\n检测到的鼠标设备:");
    for d in devices::enumerate_mice() {
        println!(
            "  {}  | VID:{}  PID:{}  | 实例:{}",
            d.name,
            d.vid.as_deref().unwrap_or("----"),
            d.pid.as_deref().unwrap_or("----"),
            d.instance_id
        );
    }
}
