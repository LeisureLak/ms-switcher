# MouseSpeedSwitcher（鼠标灵敏度配置切换）

当前版本：**v0.1.3**

## 主要功能

Windows 后台小工具：为全局环境和单个鼠标（尤其是轨迹球）保存独立的指针/滚轮配置；主菜单可在全局配置与当前已连接设备的特定配置之间切换，再次点击当前设备会切回全局配置。支持给每个设备配置设置**别名**，并可独立配置滚轮模式，按住触发键移动轨迹球即可纵向滚动。

## 软件特点

Win32 原生单文件 exe，约 650KB，无运行时依赖。完全事件驱动，CPU 近乎零开销；空闲 Private Bytes 约 1.7MB，工作集约 10.8MB。

## 使用说明

运行 exe，右键托盘图标 → 悬停「其他设备」→ 目标设备 →「用当前速度保存规则」。主菜单顶部显示当前生效的是全局配置还是设备特定配置；设备区第一项为「全局配置」，下面是已连接且有配置的设备。点击设备项切换到该设备，再次点击会切回全局配置。顶部设置区只编辑全局配置，与当前系统值及设备特定配置脱钩；「恢复默认」会把全局指针/滚轮速度恢复为 Windows 默认，并删除全局滚轮模式及其快捷键配置。

## 下载与源码

- GitHub Releases：https://github.com/LeisureLak/ms-switcher/releases
- GitHub：https://github.com/LeisureLak/ms-switcher
- Gitee：https://gitee.com/LinkToThePast/ms-switcher

版本变更详见 [CHANGELOG.md](CHANGELOG.md)。
