use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::*;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("MouseSpeedSwitcher");

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 是否已注册开机自启。
pub fn is_enabled() -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_READ, &mut key) != ERROR_SUCCESS
        {
            return false;
        }
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() as u32) * 2;
        let status = RegGetValueW(
            key,
            PCWSTR::null(),
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        status == ERROR_SUCCESS && size > 0
    }
}

/// 注册开机自启（HKCU Run 键，无需管理员权限）。
pub fn enable() -> bool {
    let path = exe_path();
    if path.is_empty() {
        return false;
    }
    let ws: Vec<u16> = format!("\"{}\"", path)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(ws.as_ptr() as *const u8, ws.len() * 2) };
    unsafe {
        let mut key = HKEY::default();
        let mut disposition = REG_CREATE_KEY_DISPOSITION::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            Some(&mut disposition),
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let r = RegSetValueExW(key, VALUE_NAME, Some(0), REG_SZ, Some(bytes));
        let _ = RegCloseKey(key);
        r == ERROR_SUCCESS
    }
}

/// 取消开机自启。
pub fn disable() -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_SET_VALUE, &mut key)
            != ERROR_SUCCESS
        {
            return false;
        }
        let r = RegDeleteValueW(key, VALUE_NAME);
        let _ = RegCloseKey(key);
        r == ERROR_SUCCESS
    }
}
