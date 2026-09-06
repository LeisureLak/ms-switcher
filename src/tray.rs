//! 托盘图标（tray-icon crate）。
//!
//! 事件通过 TrayIconEvent 全局通道转发：tray.rs 不再需要窗口回调消息，
//! 点击信号由 main.rs 的事件线程写入 app::TRAY_CLICK。

use tray_icon::TrayIcon;

pub struct TrayHandle {
    icon: TrayIcon,
}

impl TrayHandle {
    /// 添加托盘图标；失败返回 None。
    pub fn new(tip: &str) -> Option<TrayHandle> {
        let icon = make_icon()?;
        let tray = tray_icon::TrayIconBuilder::new()
            .with_tooltip(tip)
            .with_icon(icon)
            .build()
            .ok()?;
        Some(TrayHandle { icon: tray })
    }

    pub fn set_tip(&self, tip: &str) {
        let _ = self.icon.set_tooltip(Some(tip.to_string()));
    }
}

// TrayIcon 内部句柄由 crate 管理，跨线程安全。
unsafe impl Send for TrayHandle {}
unsafe impl Sync for TrayHandle {}

/// 生成一个简单的程序图标（32×32 RGBA：深蓝圆 + 浅色指针点缀）。
fn make_icon() -> Option<tray_icon::Icon> {
    const S: usize = 32;
    let mut rgba = vec![0u8; S * S * 4];
    let c = (S as f32 - 1.0) / 2.0;
    for y in 0..S {
        for x in 0..S {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = (dx * dx + dy * dy).sqrt();
            let i = (y * S + x) * 4;
            if d <= c {
                // 深蓝圆底
                let (r, g, b) = (0x00, 0x78, 0xD7);
                // 中间画一个白色小三角（模拟指针）
                let inside_triangle =
                    dy > -2.0 && dy < 8.0 && dx > -6.0 + dy * 0.45 && dx < -1.0 + dy * 0.45;
                if inside_triangle {
                    rgba[i] = 0xFF;
                    rgba[i + 1] = 0xFF;
                    rgba[i + 2] = 0xFF;
                } else {
                    rgba[i] = r;
                    rgba[i + 1] = g;
                    rgba[i + 2] = b;
                }
                rgba[i + 3] = 0xFF;
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, S as u32, S as u32).ok()
}
