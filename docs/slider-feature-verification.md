# 托盘菜单「灵敏度滑动条」—— 实现与验证问题总结

> 交接文档：功能代码已实现并通过编译/单元测试。
> **2026-09-06 更新：谜底已揭开，端到端验证已通过，遗留事项已全部清理。**
> 原始排查记录保留在下方，供回顾；结论见文首。

---

## ✅ 最终结论（2026-09-06）

「未解之谜」的根因是 **验证脚本编码问题，与 Rust 代码、PowerShell 语言本身均无关**：

- `%TEMP%` 下的脚本均为 **UTF-8 无 BOM** 编码；
- Windows PowerShell 5.1 对无 BOM 文件按系统 ANSI（本机 GBK/cp936）解码；
- 中文注释的 UTF-8 尾字节恰好是 GBK 双字节字符的前导字节（lead byte），
  与后续 `\r` 组成一个 GBK 字符，**把回车吞掉**，导致下一行代码与注释
  合并成一行——**下一行代码整行变成了注释，从未执行**。

`verify_slider.ps1` 的 GBK 视图实锤（节选）：

```
66:     # 找到主窗?    $main = [IntPtr]::Zero      ← $main 赋值被注释吞掉！
74:     # 模拟菜单点击「调节灵敏度…?    [Win]::SendMessageW(...)   ← 触发弹窗的代码被吞！
```

由此完美解释当时的全部"灵异现象"：

| 现象 | 真实原因 |
|---|---|
| `main window:` 打印为空 | `$main = ...` 赋值行被吞，`$main` 保持 `$null` |
| `if ($main -eq [IntPtr]::Zero) { throw }` 不触发 | `$null -eq [IntPtr]::Zero` 为 `False`（不是 IntPtr 比较怪癖） |
| 后续在 `EnumChildWindows($slider=null)` 处才炸 | `$slider = ...` 赋值同样被吞 |
| `diag_life.ps1` 全绿 | 它的中文注释恰好都以不吞换行的字符结尾，一行代码都没被吞 |
| "同脚本第一次失败、第二次成功"的观感 | 不同次实验用了内容不同的脚本副本，吞行情况不同 |

当时文档列出的四个怀疑点：**1（string marshaling）确有其事但与本问题无关；
2（`$script:` 闭包）经最小复现实验排除；3（`IntPtr` 比较）实为 `$null` 之差；
4（输出缓冲）是吞行导致的执行路径错乱，非并发问题。**

**修复**：把脚本转为 **UTF-8 with BOM**（`%TEMP%` 下 13 个脚本已全部转换），
原 `verify_slider.ps1` / `verify_slider2.ps1` **未改一行逻辑**，直接全绿：

```
main window: 1640338
slider window: 2819938
trackbar: 9505308
speed after slider to 7: 7
re-click reuses same window OK
speed after reset: 10
slider window closed OK
deactivate destroys window OK
ALL CHECKS PASSED
```

> 教训：在 Windows PowerShell 5.1 下运行含中文的脚本，必须存为
> **UTF-8 with BOM**（或 `pwsh` 7+ / 注释用英文）。

---

## 一、功能实现（已完成）

用户需求：在托盘菜单中添加滑动条，快捷修改系统鼠标灵敏度。

由于 Windows 原生菜单（`HMENU`）**无法嵌入控件**，采用标准做法：
托盘菜单新增「调节灵敏度…」菜单项（`IDM_SPEED_SLIDER = 903`），点击后弹出
一个小型置顶窗口（位于鼠标光标附近），内含 Trackbar（1–20），拖动实时
调用 `speed::set()`，Esc / 失焦 / 「完成」按钮关闭。

改动文件：

| 文件 | 改动 |
|---|---|
| `Cargo.toml` | windows 依赖新增 `Win32_UI_Controls`、`Win32_UI_HiDpi`、`Win32_UI_Input_KeyboardAndMouse` |
| `src/slider.rs` | **新增**：滑动条弹窗模块（窗口类 `MouseSpeedSwitcherSliderWnd`） |
| `src/lib.rs` | 导出 `slider` 模块 |
| `src/main.rs` | 菜单项 + `IDM_SPEED_SLIDER` 命令处理 + 主窗口 `WM_SETTINGCHANGE` 同步托盘提示 |

`src/slider.rs` 要点：
- 单实例：`static OPEN: Mutex<Option<SliderWnd>>`，重复点击复用已有窗口
- 控件：Static 标签（实时显示当前值）+ `msctls_trackbar32`（`TBM_SETRANGE` 1–20、`TBM_SETPOS`、`TBM_SETTICFREQ` 2）+ 「恢复默认」「完成」按钮
- 拖动事件：`WM_HSCROLL`（`TB_THUMBTRACK` 等）→ `TBM_GETPOS` → `speed::set()` 实时生效
- 关闭：Esc（`WM_KEYDOWN`）、「完成」按钮、失焦（`WM_ACTIVATE` `WA_INACTIVE`）
- 布局按 `GetDpiForWindow` 缩放；位置夹取到 `MonitorFromPoint` 工作区内
- 注：`TBM_GETPOS`（0x0400）在 windows crate 0.62.2 中未导出，代码中按 SDK 值自行定义

当前状态：
- `cargo check` / `cargo build --release` 通过
- `cargo test --release -- --test-threads=1`：5/5 通过
- ~~`src/slider.rs` 中留有临时调试日志~~ **已清理（2026-09-06）**

---

## 二、已确认的事实（有日志/实验证据）

1. **滑动条窗口可以正常创建、稳定存活。**
   用 `SendMessageW(主窗口, WM_COMMAND, 903, 0)` 模拟菜单点击后，
   `%TEMP%\mss_dbg.log` 记录：
   ```
   show: enter
   show: class ok
   show: created HWND(0x29067a)
   ```
   随后 100ms / 600ms 两次 `EnumWindows` 枚举，窗口均存在（见 `diag_life.ps1` 输出），
   说明弹窗创建、显示、存活链路正常。

2. **「失焦自动关闭」逻辑在窗口刚弹出时会被误触发（已修复）。**
   早期版本（无宽限期）的窗口消息日志显示完整销毁序列：
   ```
   show: created HWND(0x1190d72)
   ...
   wndproc: msg=0x86 wparam=0x0   ← WM_NCACTIVATE（失活）
   wndproc: msg=0x6  wparam=0x0   ← WM_ACTIVATE WA_INACTIVE
   wndproc: msg=0x2               ← WM_DESTROY
   wndproc: WM_DESTROY            ← 自己代码的销毁分支
   wndproc: msg=0x82              ← WM_NCDESTROY
   ```
   `WM_ACTIVATE(WA_INACTIVE)` 只能由系统派发（代码自身从不发送），因此
   失活事件来自系统：从 PowerShell 用 `SendMessage` 模拟点击时，进程没有
   真实用户输入权限，`SetForegroundWindow` 的激活是临时的，`SendMessage`
   返回后系统把前台归还，窗口随即收到失活通知；而我们自己的失焦销毁逻辑
   无法区分「用户点了别处」与「激活未站稳被收回」，导致窗口刚弹出即被销毁。

   **修复**：`WM_ACTIVATE` 失活销毁增加 500ms 宽限期
   （`ACTIVATE_GRACE_MS`，窗口创建时刻记在 `GWLP_USERDATA`），
   宽限期内失活不销毁。`diag_life.ps1` 验证修复生效。

---

## 三、原「未解之谜」—— 已定性（根因见文首）

完整验证脚本（`verify_slider.ps1` / `verify_slider2.ps1`）反复出现
**无法解释的失败**，而结构几乎相同的 `diag_life.ps1` **完全正常**。
怀疑问题出在 PowerShell 验证脚本一侧，而非 Rust 代码 —— **此判断正确，
但具体原因当时未找到，最终确认为脚本编码（UTF-8 无 BOM 被 GBK 误读）。**

当时的现象记录：

| 脚本 | 类名 | 找窗口方式 | 结果 |
|---|---|---|---|
| `diag_life.ps1` | `Wl` | `EnumWindows` 回调收集 | ✅ 找到主窗口 + slider 窗口，600ms 内稳定 |
| `diag_find4.ps1` | `W7` | `FindWindowW` | ✅ 仅 StringBuilder 参数版返回正确句柄 |
| `verify_slider.ps1` | `Win` | `FindWindowW`(StringBuilder) | ❌ 主窗口偶尔找不到（输出 `main window: ` 为空） |
| `verify_slider2.ps1` | `Wv` | `EnumWindows` 回调收集 | ❌ 主窗口找不到（`proc alive: True`，但枚举结果为空） |

当时的四个怀疑点（最终定性）：

1. **PowerShell 5.1 的 P/Invoke `string` 参数 marshaling 异常**
   —— 确有其事（`FindWindowW` 需用 StringBuilder），但与本问题无关。
2. **PowerShell 委托回调中的 `$script:` 作用域竞态**
   —— 经最小复现实验**排除**：函数内 `$script:` 收集在 5.1 下完全正常。
3. **`IntPtr` 的 `-eq` 比较语义不稳定**
   —— 实为 `$null -eq [IntPtr]::Zero` 为 `False` 导致 throw 分支失效，
   根源还是赋值行被吞后 `$main` 为 `$null`。
4. **exec 工具 shell 输出缓冲/竞态**
   —— 吞行导致执行路径错乱造成的观感，非并发问题。
5. **明确排除**：Rust 侧代码导致该现象。（正确，最终验证 Rust 侧全链路无问题。）

---

## 四、复现步骤

```powershell
# 1. 构建
cargo build --release

# 2. 运行验证脚本（脚本在 %TEMP% 下；已全部转为 UTF-8 with BOM）
powershell -ExecutionPolicy Bypass -File "$env:TEMP\verify_slider2.ps1"
```

脚本作用：
1. 启动 `target\release\mouse-speed-switcher.exe`
2. `EnumWindows` 找主窗口类 `MouseSpeedSwitcherWndClass`
3. `SendMessageW(main, WM_COMMAND, 903, 0)` 触发滑动条弹窗
4. 找 `MouseSpeedSwitcherSliderWnd` → 找 `msctls_trackbar32` 子控件
5. `TBM_SETPOS` + `WM_HSCROLL(TB_THUMBTRACK)` 模拟拖动到 7，
   用 `SystemParametersInfo(SPI_GETMOUSESPEED)` 断言速度变为 7
6. 断言：重复点击复用同一窗口；「恢复默认」→ 10；「完成」关闭；
   过宽限期后 `WM_ACTIVATE(WA_INACTIVE)` 自动销毁
7. finally 恢复原始速度并杀进程

**状态：全链路验证已通过（2026-09-06）。**

---

## 五、待办事项

- [x] 跑通完整链路验证（拖动→速度变化→按钮→失活关闭）—— 2026-09-06 全绿
- [x] 清理 `src/slider.rs` 临时调试日志（`dbg` 函数及调用点）—— 已删除
- [x] 更新 `README.md`（菜单项表格补充「调节灵敏度…」说明）
- [ ] 手工体验：真实点击托盘菜单，确认弹窗位置、拖动手感、失焦关闭

## 六、相关文件

- 功能代码：`src/slider.rs`、`src/main.rs`（`IDM_SPEED_SLIDER`、`WM_SETTINGCHANGE`）
- 验证脚本（`%TEMP%` 下，未入库，已全部转为 UTF-8 with BOM）：
  - `diag_life.ps1` —— 窗口创建/存活诊断
  - `verify_slider.ps1` / `verify_slider2.ps1` —— 完整链路验证（全绿）
  - `debug_slider3.ps1` —— 读 `%TEMP%\mss_dbg.log`（日志已随调试代码删除）