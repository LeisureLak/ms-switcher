use std::mem::size_of;

use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::core::GUID;

/// GUID_DEVCLASS_MOUSE = {4d36e96f-e325-11ce-bfc1-08002be10318}
const GUID_DEVCLASS_MOUSE: GUID = GUID {
    data1: 0x4d36e96f,
    data2: 0xe325,
    data3: 0x11ce,
    data4: [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18],
};

/// 一个鼠标设备的信息。
#[derive(Debug, Clone)]
pub struct Device {
    /// 设备实例 ID（唯一，含实例编号，可区分两个同型号设备）。
    pub instance_id: String,
    /// 4 位大写十六进制 VID（无则 None，例如部分蓝牙设备）。
    pub vid: Option<String>,
    /// 4 位大写十六进制 PID（无则 None）。
    pub pid: Option<String>,
    /// 设备友好名称。
    pub name: String,
}

/// 枚举当前系统中所有鼠标类设备。
pub fn enumerate_mice() -> Vec<Device> {
    let mut out = Vec::new();

    let hdev = match unsafe {
        SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_MOUSE), None, None, DIGCF_PRESENT)
    } {
        Ok(h) => h,
        Err(_) => return out,
    };
    if hdev.is_invalid() {
        return out;
    }

    let mut index: u32 = 0;
    loop {
        let mut data: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
        data.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;

        if unsafe { SetupDiEnumDeviceInfo(hdev, index, &mut data) }.is_err() {
            break;
        }
        index += 1;

        let instance_id = get_instance_id(hdev, &data).unwrap_or_default();
        let hwids = get_multi_string(hdev, &data, SPDRP_HARDWAREID);
        let (vid, pid) = parse_vid_pid(&hwids);
        let name = get_string(hdev, &data, SPDRP_FRIENDLYNAME)
            .or_else(|| get_string(hdev, &data, SPDRP_DEVICEDESC))
            .unwrap_or_else(|| instance_id.clone());

        out.push(Device {
            instance_id,
            vid,
            pid,
            name,
        });
    }

    let _ = unsafe { SetupDiDestroyDeviceInfoList(hdev) };
    out
}

fn get_instance_id(hdev: HDEVINFO, data: &SP_DEVINFO_DATA) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size: u32 = 0;
    let ok = unsafe { SetupDiGetDeviceInstanceIdW(hdev, data, Some(&mut buf), Some(&mut size)) };
    if ok.is_err() || size == 0 {
        return None;
    }
    // size 包含结尾 null，去掉它
    let len = (size as usize).min(buf.len());
    let end = if len > 0 && buf[len - 1] == 0 {
        len - 1
    } else {
        len
    };
    String::from_utf16_lossy(&buf[..end]).into()
}

/// 读取一个 REG_MULTI_SZ 类型的注册表属性（如硬件 ID 列表）。
fn get_multi_string(
    hdev: HDEVINFO,
    data: &SP_DEVINFO_DATA,
    prop: SETUP_DI_REGISTRY_PROPERTY,
) -> Vec<String> {
    let mut needed: u32 = 0;
    unsafe {
        let _ = SetupDiGetDeviceRegistryPropertyW(hdev, data, prop, None, None, Some(&mut needed));
    }
    if needed == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u16; needed as usize];
    let slice =
        unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 2) };
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(hdev, data, prop, None, Some(slice), Some(&mut needed))
    };
    if ok.is_err() {
        return Vec::new();
    }
    // REG_MULTI_SZ：以双 null 结尾的多段字符串
    let mut out = Vec::new();
    let mut cur: Vec<u16> = Vec::new();
    for &c in &buf {
        if c == 0 {
            if cur.is_empty() {
                break;
            }
            out.push(String::from_utf16_lossy(&cur));
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    out
}

/// 读取一个 REG_SZ 类型的注册表属性（如设备描述）。
fn get_string(
    hdev: HDEVINFO,
    data: &SP_DEVINFO_DATA,
    prop: SETUP_DI_REGISTRY_PROPERTY,
) -> Option<String> {
    let mut needed: u32 = 0;
    unsafe {
        let _ = SetupDiGetDeviceRegistryPropertyW(hdev, data, prop, None, None, Some(&mut needed));
    }
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    let slice =
        unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 2) };
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(hdev, data, prop, None, Some(slice), Some(&mut needed))
    };
    if !ok.is_ok() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len]).into()
}

/// 从硬件 ID 列表（形如 `HID\VID_046D&PID_C52B&MI_02`）中解析 VID/PID。
fn parse_vid_pid(hwids: &[String]) -> (Option<String>, Option<String>) {
    for id in hwids {
        let up = id.to_ascii_uppercase();
        let mut vid = None;
        let mut pid = None;
        if let Some(i) = up.find("VID_") {
            if let Some(h) = take_hex4(&up[i + 4..]) {
                vid = Some(h);
            }
        }
        if let Some(i) = up.find("PID_") {
            if let Some(h) = take_hex4(&up[i + 4..]) {
                pid = Some(h);
            }
        }
        if vid.is_some() && pid.is_some() {
            return (vid, pid);
        }
    }
    (None, None)
}

fn take_hex4(s: &str) -> Option<String> {
    let h: String = s.chars().take(4).collect();
    if h.len() == 4 && h.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(h)
    } else {
        None
    }
}
