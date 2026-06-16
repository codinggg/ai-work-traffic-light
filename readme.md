## ai work traffic light

这是一个ai工作提醒的应用

使用平台，windows，linux，macOs

使用场景是，用户使用claude 处理任务时，有时候claude要求用户确认的时候，用户不知道，所以我想开发一个桌面应用，模式就是用红绿灯来代替

桌面或者任务栏显示一个红绿灯的组件。
1. 当claude 正在工作的时候，显示绿灯
2. 当需要用户交互的时候，显示红灯
3. 当交互结束之后，继续显示绿灯
黄灯呢？该展示什么情况？

这个该如何知道使用claude 的状态，用户安装了claude，vscode中也是用claude code。

---

## 实现说明（v1）

用 **Tauri (Rust)** 做的 Windows 桌面常驻应用，通过 **Claude Code 官方 hooks** 感知状态：

- 🟢 **绿灯**：Claude 正在工作（可以放心离开）
- 🟡 **黄灯**：Claude 完成了这一轮 —— 该你了（不紧急）
- 🔴 **红灯**：Claude 中途卡住、在等你确认（紧急，弹系统通知 + 可选提示音）
- ⚫ **隐藏**：没有任何 Claude Code 会话在跑

灯是一个常驻置顶的悬浮小窗，停在任务栏左下角（天气组件附近）。同时开多个会话时，灯显示最紧急的那个，并在红灯时标出是哪个项目在等你。

> v1 仅支持 Windows；macOS/Linux、以及"点灯直接跳到对应 VSCode 窗口"已在计划中、后续实现。
> 设计与计划见 [docs/brainstorms/](docs/brainstorms/) 与 [docs/plans/](docs/plans/)。

## 前置

- [Rust](https://rustup.rs/)（stable toolchain）
- Node.js + [pnpm](https://pnpm.io/)
- Windows 10/11（自带 WebView2）

## 开发运行

```bash
pnpm install
pnpm tauri dev      # 首次会编译 Rust 依赖，稍久
```

打包：`pnpm tauri build`

## 启用状态检测

1. 启动 app 后，右键**托盘图标** → **安装 hooks**（把上报 hooks 合并写入全局 `~/.claude/settings.json`，幂等、自动备份）
2. **重启 Claude Code**（hooks 在会话启动时加载）
3. 正常使用 Claude Code，灯就会随状态变化

托盘菜单还提供：**提示音**开关、**开机自启**开关、**卸载 hooks**、**退出**。

## 验证 hook 行为

`spike/` 下有一个 hook 事件日志工具，可实测 Claude Code 各事件的触发时机与负载，用于校准事件→状态映射。见 [spike/README.md](spike/README.md)，结果记到 [docs/hook-spike-findings.md](docs/hook-spike-findings.md)。

## 修改记录

增加可以移动窗口的功能
右键红绿灯小图标，增加个锁定位置的选项，默认没有勾选，可以移动红绿灯的位置，锁定之后，不可以选中红绿灯

增加功能，点击红绿灯的托盘图标，会显示红绿灯的窗口，再点击才会隐藏窗口，不然没有claude 工作就隐藏找不到了


1，点击之后可能是被任务栏挡住了，看不到 2. 红绿灯可以在整个桌面上拖动

切换新分支，实现红绿灯的最外面显示一个小的黑色的框，默认显示3个灯，灯的颜色是很浅的，某个灯亮起后，灯的颜色是比较深的，这样区分