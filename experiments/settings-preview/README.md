# GPUI Kit 独立设置页样板

验证日期：2026-10-26，macOS。本目录是独立 Cargo package／workspace，使用自己的锁文件；主终端仍使用 GPUI 0.2.2。

## 运行

从 Next Terminal 项目根目录进入本目录后运行：

```sh
cd experiments/settings-preview
cargo run --locked
```

需要 Rust 和完整 Xcode／Metal 工具链。此目录的 `.cargo/config.toml` 将构建输出放到主项目忽略的 `target/settings-preview/`；请在本目录执行命令，使该配置生效。Cmd+W、Cmd+Q 或系统关闭按钮均可退出样板。

## 已实现

- 使用 GPUI Kit 的 Select、Slider、Button 与主题系统构建原生窗口，没有 WebView。
- 可搜索的本机字体族列表，本机验证时为 261 个，默认 Menlo。
- 12–28 逻辑像素字号，滑轨与加减按钮；数值变化立即刷新预览。
- 石墨黑／纸白主题，同时切换组件主题与预览配色。
- 中英文、数字、组合字符、颜色、选区和命令行样式的固定文本预览。
- 恢复默认，统一复位字体、字号、主题及控件显示值。

控件状态保存在 `SettingsPreview` 中。字体和主题确认事件更新预览；字号观察 SliderState 的值变化。预览只读取这几项状态并重绘，不连接 PTY，也不读写终端配置。

## 兼容性结论

| 项目 | 固定依赖 | 结论 |
|---|---|---|
| 主终端 | `gpui = 0.2.2` | 保持原样 |
| 此样板 | `gpui-kit = 0.8.0` → `gpui-pre = 0.3.9` | 独立构建与原生运行通过 |
| 将 Kit Button 直接放进旧版 GPUI 元素树 | 同时启用两套 GPUI | 编译失败：两套 `IntoElement` 类型不相同 |

两套依赖可以被 Cargo 解析到同一依赖图，但不能因此互换 Window、Entity 或 UI 元素。未来接入主程序需要另做 GUI 依赖与 API 迁移验证，本轮没有执行迁移。

可复现的负向编译探针：

```sh
cargo check --locked --features compat-old-gpui
```

**该命令预期失败，退出码 101**。这个显式功能只用于诊断，默认关闭，不是样板正常构建所需的功能。实际错误为：

```text
error[E0277]: the trait bound
`gpui_kit::gpui_component::button::Button: gpui::IntoElement` is not satisfied
note: there are multiple different versions of crate `gpui` in the dependency graph
```

正常构建不启用此功能：

```sh
cargo fmt --all --check
cargo build --locked
```

版本依据：[GPUI Kit 0.8.0 manifest](https://github.com/longbridge/gpui-kit/blob/v0.8.0/crates/kit/Cargo.toml)、[对应 workspace 依赖](https://github.com/longbridge/gpui-kit/blob/v0.8.0/Cargo.toml)，并核对了实际下载的 crates.io 包与独立锁文件。

## 实际验证与限制

原生窗口验证了字体搜索 `Monaco` 并选择后字形变化、字号加减、点击滑轨从 16 调至 24、石墨黑切换纸白，以及恢复默认回到 Menlo／16／石墨黑。控件值和预览同步变化；中文与组合字符正常显示。构建与格式检查通过。

自动化未确认连续拖动或通过 AXValue 直接设置滑块的操作效果，拖动手感与辅助功能调节仍待人工验证。本轮不宣称全套键盘／辅助功能兼容性。

预览是固定样例，不是 Alacritty 字符网格或真实 shell 输出；选区和光标也是展示样式。字号和字体仅影响预览，设置页自身字号保持稳定。字体列表尚未筛选等宽字体，实际终端采用前需验证字宽与字体回退。

本轮没有持久化、保存按钮、主终端设置联动、主题导入或主程序迁移。设置只存在于本次窗口，关闭后回到默认值。

实现中发现 Kit Select／Slider 的外层会填满父容器，已为它们设置固定控件行高度，避免挤占布局。GPUI 的 `on_window_closed` 在这套依赖中也与主项目旧接口不同，进一步说明不能直接混用。
