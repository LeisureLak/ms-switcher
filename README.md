# MouseSpeedSwitcher（鼠标灵敏度自动切换）

一个极小、极快的 Windows 后台工具：**识别系统中的鼠标设备，在插入特定鼠标（如轨迹球）时自动切换指针速度，拔出时恢复原速度**。

例如：你的轨迹球鼠标厂商没提供灵敏度调节，而 Windows 的默认速度对它来说太快。给它的 `VID:PID` 配一条规则后，插入轨迹球 → 自动降到合适速度；拔出 → 自动恢复。

## 特性

- **内存极小**：私有内存约 **1.8 MB**（工作集约 12 MB）
- **启动极快**：无运行时依赖，启动 < 10 ms
- **CPU 占用 0%**：纯事件驱动，平时零开销；设备插拔事件去抖 400 ms 后处理，另有 30 秒兜底轮询防止漏事件
- **托盘菜单**：列出所有鼠标设备（名称 + VID/PID + 实例号），带规则的设备直接在列表项上标注 `规则 N`，当前生效的还会标 `✓生效中`；多个规则设备并存时菜单顶部标明生效者。一键「用当前速度保存规则」「删除规则」「重新应用规则」，无需手改配置
- **开机自启**：写入 HKCU Run 键，无需管理员权限，托盘菜单可开关
- **单文件** exe（约 180 KB），普通用户权限即可运行

## 使用

1. 运行 `mouse-speed-switcher.exe`（托盘出现图标）。
2. **先把系统指针速度调到普通鼠标（非轨迹球）时的值**。
3. 插入轨迹球。
4. 把系统指针速度调到轨迹球想要的速度。
5. 托盘图标 → 鼠标设备 → 找到轨迹球（如 `ELECOM Trackball Mouse`）→ **用当前速度保存规则**。
6. 完成。之后插入轨迹球自动切到该速度，拔出自动恢复第 2 步的值。

其它菜单项：

| 菜单项 | 作用 |
|---|---|
| 调节灵敏度… | 弹出滑动条（1–20）实时调节当前鼠标指针速度，拖动即时生效；Esc / 点别处 / 「完成」关闭，「恢复默认」回到 10 |
| 开机自启 | 开关登录自启动 |
| 重新加载配置 | 手改 `config.json` 后立即生效 |
| 退出 | 退出程序（已应用的速度保持不动） |

## 配置文件

位于 `%APPDATA%\MouseSpeedSwitcher\config.json`，由托盘菜单自动维护，也可手改：

```json
{
  "rules": [
    {
      "vid": "056E",
      "pid": "01C5",
      "speed": 4,
      "note": "ELECOM 轨迹球"
    }
  ]
}
```

- `vid` / `pid`：设备硬件 ID 中的 4 位十六进制（大小写不敏感）。可通过托盘菜单查看每个设备的 VID/PID，或运行 `diag.exe` 查看全部设备。
- `speed`：Windows 指针速度滑块档位（1–20，默认 10）。
- `note`：备注，仅用于标识。

## 工作原理

- **设备监听**：`RegisterDeviceNotification` 监听鼠标设备接口（`GUID_DEVINTERFACE_MOUSE`）的插拔，配合隐藏窗口接收 `WM_DEVICECHANGE`，去抖后全量重扫。
- **设备识别**：`SetupAPI` 枚举鼠标设备类（`GUID_DEVCLASS_MOUSE`），从硬件 ID（如 `HID\VID_056E&PID_01C5&MI_00`）解析 VID/PID；设备实例 ID 用于区分同型号多设备。
- **速度切换**：`SystemParametersInfo(SPI_SETMOUSESPEED)`，值通过 `pvParam` 传递（`uiParam` 必须为 0），带 `SPIF_UPDATEINIFILE | SPIF_SENDCHANGE`。此 API 只更新会话内的滑块值，**不会改动**注册表中的增强指针精度配置。
- **恢复策略**：每个规则设备插入瞬间记录当时的滑块值，拔出时若无其它规则设备则恢复该值；若有则应用最后插入的规则设备的速度。

## 构建

```sh
cargo build --release
```

产物：`target\release\mouse-speed-switcher.exe`（主程序）、`target\release\diag.exe`（诊断工具）。

诊断工具输出示例：

```
当前指针速度: 4
配置文件: C:\Users\lak\AppData\Roaming\MouseSpeedSwitcher\config.json
已配置规则: 1
  VID:056E PID:01C5 -> 速度 4 ELECOM 轨迹球

检测到的鼠标设备:
  ELECOM Trackball Mouse  | VID:056E  PID:01C5  | 实例:HID\VID_056E&PID_01C5&MI_00\7&23539E1A&0&0000
  ...
```

## 测试

```sh
cargo test --release -- --test-threads=1
```

状态机单测会真实调用系统 API 修改滑块值，结束后自动恢复，故要求串行执行。

## 已知限制

- **蓝牙鼠标**：部分蓝牙 HID 设备的硬件 ID 不含 VID/PID（形如 `BTHENUM\...`），无法按 VID/PID 匹配规则，托盘菜单中会显示 `VID:---- PID:----` 并禁用规则项。
- 指针速度是系统全局设置，无法按设备区分，因此规则基于"插入/拔出"事件切换；两个带规则设备同时插入时，后插入者生效，拔出后回退到剩余规则或插入前值。
