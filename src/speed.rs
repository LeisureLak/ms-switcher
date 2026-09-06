use std::ffi::c_void;

use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETMOUSESPEED, SPI_GETWHEELSCROLLLINES, SPI_SETMOUSESPEED,
    SPI_SETWHEELSCROLLLINES, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

/// 滚轮速度范围（行/齿）。Windows 设置 UI 的取值范围是 1–100，默认 3；
/// 注意 Windows 没有按设备区分滚轮速度的系统 API，此值为全局。
pub const WHEEL_MIN: u32 = 1;
pub const WHEEL_MAX: u32 = 100;

/// Windows 默认指针速度（1–20 的中位）。
pub const SPEED_DEFAULT: u32 = 10;
/// Windows 默认滚轮速度（行/齿）。
pub const WHEEL_DEFAULT: u32 = 3;

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

/// 读取当前滚轮速度（1-100 行/齿，Windows 设置中的滑块，默认 3）。
pub fn get_wheel() -> u32 {
    let mut v: u32 = 0;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some(&mut v as *mut u32 as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    // WHEEL_PAGESCROLL（每齿滚一屏）在设置 UI 之外；这里夹到常规范围
    v.clamp(WHEEL_MIN, WHEEL_MAX)
}

/// 设置滚轮速度（自动夹到 1-100 行/齿），即时生效并持久化。
///
/// 注意：SPI_SETWHEELSCROLLLINES 的行数通过 uiParam 传递，pvParam 必须为 NULL。
pub fn set_wheel(v: u32) {
    let lines = v.clamp(WHEEL_MIN, WHEEL_MAX);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETWHEELSCROLLLINES,
            lines,
            None,
            SPIF_SENDCHANGE | SPIF_UPDATEINIFILE,
        );
    }
}
