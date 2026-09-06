use std::ffi::c_void;

use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETMOUSESPEED, SPI_SETMOUSESPEED, SPIF_SENDCHANGE,
    SPIF_UPDATEINIFILE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

/// 读取当前指针速度（1-20，对应 Windows 设置中的滑块，默认 10）。
pub fn get() -> u32 {
    let mut v: u32 = 0;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETMOUSESPEED,
            0,
            Some(&mut v as *mut u32 as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    v
}

/// 设置指针速度（自动夹到 1-20），即时生效并持久化。
///
/// 注意：SPI_SETMOUSESPEED 的速度值通过 pvParam（作为整数指针）传递，uiParam 必须为 0。
pub fn set(v: u32) {
    let speed = v.clamp(1, 20);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETMOUSESPEED,
            0,
            Some(speed as usize as *mut c_void),
            SPIF_SENDCHANGE | SPIF_UPDATEINIFILE,
        );
    }
}
