//! RegisterDeviceNotificationW：鼠标接口设备到达/移除通知。
//!
//! 注册到宿主顶层 HWND，`WM_DEVICECHANGE` 由 host.rs 处理并防抖。
//! HDEVNOTIFY 用 RAII 管理，Drop 时反注册。

use windows::Win32::Devices::HumanInterfaceDevice::GUID_DEVINTERFACE_MOUSE;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::UI::WindowsAndMessaging::{
    RegisterDeviceNotificationW, UnregisterDeviceNotification, DBT_DEVTYP_DEVICEINTERFACE,
    DEV_BROADCAST_DEVICEINTERFACE_W, DEVICE_NOTIFY_WINDOW_HANDLE, HDEVNOTIFY,
};

/// 鼠标接口设备通知（GUID_DEVINTERFACE_MOUSE）。
pub struct MouseDevNotify {
    handle: HDEVNOTIFY,
}

impl MouseDevNotify {
    /// 对宿主窗口注册；失败返回 None（此时上层应评估启用低频兜底扫描）。
    pub fn new(hwnd: HWND) -> Option<MouseDevNotify> {
        let mut filter = DEV_BROADCAST_DEVICEINTERFACE_W {
            dbcc_size: std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
            dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
            dbcc_reserved: 0,
            dbcc_classguid: GUID_DEVINTERFACE_MOUSE,
            dbcc_name: [0],
        };
        let handle = unsafe {
            RegisterDeviceNotificationW(
                HANDLE(hwnd.0),
                &mut filter as *mut _ as *const core::ffi::c_void,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            )
        }
        .ok()?;
        Some(MouseDevNotify { handle })
    }
}

impl Drop for MouseDevNotify {
    fn drop(&mut self) {
        unsafe {
            let _ = UnregisterDeviceNotification(self.handle);
        }
    }
}
