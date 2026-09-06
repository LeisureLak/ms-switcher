//! Win32 集中层：窗口宿主、托盘、（后续阶段）设备通知与菜单控件。
//!
//! 原则：纯模型（menu_model / state 等）不依赖本层；本层只把 OS 事件
//! 映射为模型更新并执行模型动作；FFI 资源所有权集中在本层管理。

pub mod device_notify;
pub mod host;
pub mod menu;
pub mod submenu;
pub mod tray;
