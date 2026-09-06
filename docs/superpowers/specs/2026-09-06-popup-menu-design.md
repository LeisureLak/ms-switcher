# 自绘托盘菜单（内嵌滑动条）设计

日期：2026-09-06
状态：已与用户确认

## 需求

1. 用户右键托盘图标弹出的菜单里**直接内嵌灵敏度滑动条**，拖动实时生效，不再单独弹窗。
2. 菜单其余功能（设备列表、规则操作、开机自启、重新加载配置、退出）保持不变。
3. 修复高分屏模糊：进程声明 PerMonitorV2 DPI 感知。

## 背景

- Windows 原生 HMENU（TrackPopupMenu）无法嵌入交互控件，这是系统限制。
- 因此用「自绘菜单弹窗」整体替换 TrackPopupMenu：一个 WS_POPUP 置顶窗口，
  外观模仿原生菜单（系统菜单字体、COLOR_MENU 背景、COLOR_HIGHLIGHT 悬停），
  顶部为滑动条区，下方为自绘菜单项。
- 现程序从未声明 DPI 感知（无 SetProcessDpiAwareness 调用、无 manifest），
  系统对整个进程做位图拉伸虚拟化 → 高分屏下一切模糊。菜单内的
  GetDpiForWindow 缩放逻辑因此从未真正生效。

## 架构

```
托盘点击 → popup_menu::show(hwnd)
             ├─ 收集数据：speed::get()、devices::enumerate_mice()、规则状态
             ├─ 组装 Vec<Item>（纯数据，可单测）
             ├─ 计算布局（纯函数，可单测）→ 窗口宽高
             └─ 创建 WS_POPUP|WS_BORDER|WS_VISIBLE 窗口
```

### 模块划分

| 模块 | 职责 |
|---|---|
| `src/popup_menu.rs`（新增） | 自绘菜单窗口：项列表构建、布局计算、绘制、悬停/点击、滑动条区 |
| `src/main.rs` | 托盘点击改调 `popup_menu::show`；菜单命令逻辑抽成 `run_menu_command(cmd)` 复用；`main()` 开头声明 DPI 感知 |
| `src/slider.rs` | **删除**（职责并入 popup_menu） |

### Item 模型（纯数据，便于单测）

```rust
enum ItemKind {
    DeviceTitle { name: String, vid_pid: String },
    SubAction  { cmd: u32, enabled: bool, danger: bool },  // 设备子项
    Checkable  { cmd: u32, checked: bool },                // 开机自启
    Action     { cmd: u32 },                               // 重新加载/退出
    Separator,
    Disabled   { text: String },                           // 灰字说明行（设备标题下的 VID:PID 等）
}
struct Item { kind: ItemKind, text: String, y: i32, h: i32 }  // y/h 布局计算产物
```

命令 ID 沿用 main.rs 现有 IDM_* 体系（设备子项走 IDM_DEV_BASE 槽位），
点击时直接调用现有 `handle_menu_command` 分支 → 行为与旧菜单完全一致。

### 窗口结构

- 父窗口（自绘菜单）：`WS_POPUP | WS_BORDER | WS_VISIBLE`，`WS_EX_TOPMOST | WS_EX_TOOLWINDOW`
- 子控件（仅滑动条区）：
  - Static 标签「指针速度: N」
  - Trackbar（1–20，TBM_AUTOTICKS，拖动 → WM_HSCROLL → speed::set 实时生效）
  - Button「恢复默认」（→ 速度 10）
- 菜单项区：父窗口 WM_PAINT 自绘（DrawText + FillRect 高亮），无子控件

### 布局（96 DPI 基准，创建时按 GetDpiForWindow 缩放）

```
┌─ 弹窗 ──────────────────────┐
│ 指针速度: 7        [恢复默认] │ ← 标题区（Static + Button 右对齐）
│ ●───────────■─────────      │ ← Trackbar 区
│ ─────────────────────────── │ ← 分隔
│ 轨迹球 [046d:c52b]           │ ← 每设备：标题行
│ VID:046d  PID:c52b          │ ← 灰字信息行（Disabled）
│ 用当前速度保存规则           │ ← SubAction（enabled 按规则状态）
│ 删除此设备规则               │
│ 重新应用规则                 │
│ ─────────────────────────── │
│ 开机自启                ✓   │ ← Checkable
│ 重新加载配置                 │
│ 退出                         │
└─────────────────────────────┘
```

- 设备子项**悬停展开**（2026-09-06 应用户要求从平铺改为二级展开，与原生菜单行为一致：
  悬停设备标题行展开其子项，移开收起，展开时窗口高度动态调整）。
- 恢复旧原生菜单的展示逻辑（2026-09-06 用户要求保留）：
  - 设备行文本带规则标记：`ELECOM 轨迹球 [046d:c52b] · 规则 4 ✓生效中`；
  - 生效中的规则设备行画勾选标记（✓）；
  - 两个以上规则设备并存时，菜单顶部一行灰字说明当前生效者（`生效规则: name (速度 sp)`）；
  - 托盘 tooltip 不变：`鼠标灵敏度切换 - 当前指针速度: N[ · 生效规则: name (速度 sp)]`。
- 无规则设备：「删除此设备规则」置灰（与旧菜单一致）；其余子项可用。
- 无 VID/PID 设备：三个子项全部置灰。
- 无设备：显示「未检测到鼠标设备」灰字。
- 宽度固定基准 320（含 DPI 缩放）；每行高度按菜单字体度量计算。

### 交互

| 事件 | 行为 |
|---|---|
| WM_MOUSEMOVE | 命中测试 → 悬停高亮（仅可点项）；TrackMouseRect 注册 WM_MOUSELEAVE |
| WM_MOUSELEAVE | 清除悬停并重绘 |
| WM_LBUTTONUP | 命中可点项 → 执行命令；「退出」或设备命令后关闭弹窗 |
| WM_HSCROLL | 拖动实时 speed::set + 更新标签 |
| 「恢复默认」 | TBM_SETPOS(10) + speed::set(10) |
| WM_KEYDOWN Esc | 关闭 |
| WM_ACTIVATE WA_INACTIVE | 过 500ms 宽限期后销毁（沿用 slider.rs 已验证模式，GWLP_USERDATA 记创建时刻） |
| 速度变化 | WM_SETTINGCHANGE → update_tip（现有逻辑） |

### DPI

- `main()` 开头：`SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`（失败则继续，旧系统兼容回退无需处理——GetDpiForWindow 在无感知进程返回 96，行为退化为现状）。
- 布局尺寸统一经 `s(v) = v * dpi / 96` 缩放（沿用 slider.rs 公式）。
- 菜单字体：`SystemParametersInfoW(SPI_GETNONCLIENTMETRICS)` 取 `lfMenuFont`，CreateFontIndirectW。

## 错误处理

- 窗口创建失败：静默返回（同现有 slider）。
- 枚举失败/空列表：灰字占位，不崩溃。
- 单实例：static Mutex<Option<HWND>> 复用/防重入（同现有 slider）。

## 测试

1. **单元测试**（纯函数）：Item 列表构建（有/无规则、有/无 VID:PID、无设备）、布局计算（总高 = 各行高之和；y 单调递增）、命中测试。
2. **端到端**（沿用 %TEMP% 验证脚本模式，UTF-8 BOM）：启动 exe → 发托盘模拟点击 → 枚举窗口断言弹窗可见 → 找 Trackbar 子控件 → TBM_SETPOS + WM_HSCROLL 断言速度变化 → 模拟点「恢复默认」→ Esc 关闭 → 失焦销毁。
3. cargo check / cargo test --release 全绿。