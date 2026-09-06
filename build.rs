//! 构建脚本：为可执行文件嵌入 comctl32 v6 清单与应用图标资源。
//!
//! - 没有清单时 Trackbar 等公共控件按 Windows 经典样式渲染；声明对
//!   Common-Controls v6 的依赖后获得视觉样式（现代主题外观）。
//! - `assets/icon.ico` 提供资源管理器/任务栏显示的 exe 图标；
//!   生成脚本见 `tools/gen_icon.py`。

use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // new_manifest 默认带 Common-Controls v6 依赖与兼容性声明
    embed_manifest(new_manifest("MouseSpeedSwitcher")).expect("embed manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("embed icon resource");
    }
}
