# 命令框自动恢复的边界

本轮目标：本机 zsh 的完整输入切换闭环。设置页迁移、界面重做、多会话和远程集成不在本轮范围内。

## 数据流

```text
GPUI 输入 / Alacritty 查询回复 → 有序 PTY 队列 → shell / 前台程序
PTY 原始输出 → Alacritty 网格 + ShellObserver（只观察）→ GPUI
zsh ZLE ← 会话私有 Unix socket → 同一个 PTY worker → InputControl → GPUI 焦点与布局
```

会话私有目录权限由 tempfile 控制，socket 使用随机令牌握手。它用于隔离普通终端输出和控制消息，不是对同一用户下恶意进程的安全隔离；不开放网络端口，不创建独立后台服务。控制通道不承载命令文本，不从 PTY 注入探测按键。

## 输入授权

1. shell 通过独立通道报告编辑上下文，只有空主提示符是自动恢复候选。
2. 应用发送带请求号和输入代次的 CHECK，ZLE 在处理 socket 可读事件时检查 CONTEXT、BUFFER、PREBUFFER 和待处理输入。
3. READY 只有在请求仍有效、输入代次未变化、会话存活、无备用屏幕且用户允许自动模式时才授权命令框。
4. 提交时再以输入代次为条件原子入队；一次授权只能提交一次。任何 PTY 写入（包括终端查询回复）都会推进输入代次。
5. 旧回复、重复回复、超时、断线、退出都不能重新授权。没有集成的 shell 保留显式手动确认路径。

PTY 写入和控制请求共用有序队列，代次在所有 Input 克隆之间共享。提交操作在 worker 内先设置收起命令框后的 PTY 尺寸，再写入命令，避免命令刚启动时读到旧尺寸。独立通道与 PTY 输出可以异步到达，因此不以屏幕提示符文字或单个 OSC 标记推断可提交状态。

## 用户行为

- 默认允许自动命令框。提交后立即交还直接交互，真正回到空主提示符并完成 CHECK 后恢复。
- 续行、密码式输入、vared、Vim 等前台交互保持直接输入；草稿不因切换丢失。
- Esc / Cmd+Shift+E 主动交出命令框时锁定普通终端模式，后续提示符不抢回焦点。再次 Cmd+Shift+E 恢复自动模式并重新检查。
- 切换模式时仍同步焦点、终端行列和 PTY 尺寸；复制输出不改变输入权限。

## 验收范围

纯状态测试覆盖过期、重复、代次变化、模式锁定、备用屏幕和生命周期。
真实 PTY 测试覆盖握手、CHECK、执行、续行、无回显输入、全屏程序和退出；原生窗口验证连续提交、自动恢复、草稿保留、普通终端锁定及尺寸同步。

本轮已实现并完成 34 个单元测试、5 个真实 PTY 集成测试及原生窗口检查；详细验证范围和限制见 README。协议依赖 zsh 的 [Unix socket 模块](https://zsh.sourceforge.io/Doc/Release/Zsh-Modules.html) 和 [ZLE fd handler](https://zsh.sourceforge.io/Doc/Release/Zsh-Line-Editor.html)，当前平台边界为 macOS。初始化失败不提交启动环境修改，按原配置启动普通 shell。

## 界面分层

```text
Theme（全局设计令牌） ─┬→ TerminalWindow（组合与输入路由）
                      │     ├ 标题栏：当前目录 · shell
                      │     ├ 网格：grid::layout（纯函数）→ grid::paint（GPUI）
                      │     ├ 命令框卡片：CommandEditor
                      │     └ 状态栏：chrome::tone + InputControl::label
                      └→ CommandEditor（读取同一主题）
```

- 颜色、字体与尺寸只在 `theme.rs` 定义；组件不写十六进制颜色或像素常量。主题是 GPUI Global，视图通过 `observe_global` 在主题替换时重新测量格宽并重排，为以后的设置页和浅色主题留出入口。
- 网格行列由 `theme::Metrics` 扣除标题栏、状态栏、网格留白和命令框卡片后计算，布局使用同一组数值，保证 PTY 尺寸与绘制区域一致。
- `grid::layout` 不依赖窗口，可直接用 Alacritty `Term` 做单元测试：背景按行合并，同样式连续字符合并成文字段，中文按两格宽合并，组合字符单独定位；块光标下的文字在布局阶段换成反色。`grid::paint` 只负责把计划交给 GPUI，并把所有段对齐到主等宽字体的基线。
- 界面外框只读取 `InputControl` 与 `ShellObserver` 的状态，不改变输入权限。OSC 7 当前目录与 OSC 133 一样可能被前台程序伪造，因此只用于展示。
