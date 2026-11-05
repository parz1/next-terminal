use gpui_kit::component::{
    ActiveTheme, Theme, ThemeMode,
    button::Button,
    select::{SearchableVec, Select, SelectEvent, SelectState},
    slider::{Slider, SliderState, SliderValue},
};
use gpui_kit::*;

gpui_kit::actions!(settings_preview, [Quit]);

// Deliberately fails when enabled: a Kit widget is not an old-GPUI element.
#[cfg(feature = "compat-old-gpui")]
fn compatibility_probe() {
    fn needs_original_gpui(_: impl old_gpui::IntoElement) {}
    needs_original_gpui(Button::new("compatibility-probe"));
}

type FontSelect = SelectState<SearchableVec<SharedString>>;
type ThemeSelect = SelectState<Vec<SharedString>>;
struct SettingsPreview {
    font_select: Entity<FontSelect>,
    size_slider: Entity<SliderState>,
    theme_select: Entity<ThemeSelect>,
    font_family: SharedString,
    font_size: f32,
    dark: bool,
    default_font: SharedString,
    font_count: usize,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPreview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut fonts = window.text_system().all_font_names();
        fonts.sort_unstable();
        fonts.dedup();
        if fonts.is_empty() {
            fonts.push(".SystemUIFont".to_string());
        }
        let default_index = fonts.iter().position(|font| font == "Menlo").unwrap_or(0);
        let default_font: SharedString = fonts[default_index].clone().into();
        let font_count = fonts.len();
        let font_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(
                    fonts
                        .into_iter()
                        .map(SharedString::from)
                        .collect::<Vec<_>>(),
                ),
                Some(gpui_kit::component::IndexPath::default().row(default_index)),
                window,
                cx,
            )
            .searchable(true)
        });
        let size_slider = cx.new(|_| {
            SliderState::new()
                .min(12.)
                .max(28.)
                .step(1.)
                .default_value(16.)
        });
        let theme_select = cx.new(|cx| {
            SelectState::new(
                vec![SharedString::from("石墨黑"), SharedString::from("纸白")],
                Some(gpui_kit::component::IndexPath::default().row(0)),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe(
                &font_select,
                |this, _, event: &SelectEvent<SearchableVec<SharedString>>, cx| {
                    if let SelectEvent::Confirm(Some(font)) = event {
                        this.font_family = font.clone();
                        cx.notify();
                    }
                },
            ),
            cx.observe(&size_slider, |this, slider, cx| {
                // All changes, including native accessibility adjustments and programmatic reset.
                if let SliderValue::Single(size) = slider.read(cx).value() {
                    this.font_size = size;
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &theme_select,
                window,
                |this, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                    if let SelectEvent::Confirm(Some(theme)) = event {
                        this.dark = theme.as_ref() == "石墨黑";
                        Theme::change(
                            if this.dark {
                                ThemeMode::Dark
                            } else {
                                ThemeMode::Light
                            },
                            Some(window),
                            cx,
                        );
                        cx.notify();
                    }
                },
            ),
        ];
        Self {
            font_select,
            size_slider,
            theme_select,
            font_family: default_font.clone(),
            font_size: 16.,
            dark: true,
            default_font,
            font_count,
            _subscriptions: subscriptions,
        }
    }

    fn adjust_size(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.font_size = (self.font_size + delta).clamp(12., 28.);
        self.size_slider.update(cx, |slider, cx| {
            slider.set_value(self.font_size, window, cx)
        });
        cx.notify();
    }
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.font_family = self.default_font.clone();
        self.font_size = 16.;
        self.dark = true;
        self.font_select.update(cx, |select, cx| {
            select.set_selected_value(&self.default_font, window, cx)
        });
        self.size_slider
            .update(cx, |slider, cx| slider.set_value(16., window, cx));
        self.theme_select.update(cx, |select, cx| {
            select.set_selected_value(&"石墨黑".into(), window, cx)
        });
        Theme::change(ThemeMode::Dark, Some(window), cx);
        cx.notify();
    }

    fn preview(&self) -> impl IntoElement {
        let (background, foreground, muted, accent, green, blue, selection) = if self.dark {
            (
                0x191b20, 0xe4e6eb, 0x8d93a1, 0xe4b566, 0x90bd93, 0x8daee0, 0x354b6a,
            )
        } else {
            (
                0xfbfaf7, 0x2c313a, 0x777b84, 0x9b6a27, 0x367548, 0x365fa3, 0xd9e5f3,
            )
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(background))
            .text_color(rgb(foreground))
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(if self.dark { 0x363942 } else { 0xdeddd7 }))
            .child(
                div()
                    .h(px(42.))
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(if self.dark { 0x30333a } else { 0xe8e6e0 }))
                    .text_size(px(12.))
                    .text_color(rgb(muted))
                    .child("Next Terminal")
                    .child("预览"),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p(px(24.))
                    .overflow_hidden()
                    .font_family(self.font_family.clone())
                    .text_size(px(self.font_size))
                    .line_height(px(self.font_size * 1.6))
                    .child(
                        div()
                            .text_color(rgb(muted))
                            .child("~/Projects/Next/next-terminal"),
                    )
                    .child(div().child("❯ cargo test"))
                    .child(div().h(px(14.)))
                    .child(
                        div()
                            .text_color(rgb(green))
                            .child("✓ input control · 5 passed"),
                    )
                    .child(div().child("中文 / 日本語 / English"))
                    .child(div().child("0123456789  [] {} ()  →  é"))
                    .child(div().h(px(14.)))
                    .child(
                        div()
                            .flex()
                            .gap(px(self.font_size))
                            .child(div().text_color(rgb(blue)).child("src/"))
                            .child(div().child("Cargo.toml")),
                    )
                    .child(
                        div()
                            .flex()
                            .child(div().bg(rgb(selection)).child("选区颜色示例")),
                    ),
            )
            .child(
                div()
                    .mx(px(22.))
                    .mb(px(20.))
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(rgb(if self.dark { 0x363942 } else { 0xdeddd7 }))
                    .font_family(self.font_family.clone())
                    .text_size(px(self.font_size))
                    .line_height(px(self.font_size * 1.6))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().text_color(rgb(accent)).child("❯"))
                            .child("echo 你好")
                            .child(div().w(px(2.)).h(px(self.font_size + 3.)).bg(rgb(accent))),
                    ),
            )
    }
}

impl Render for SettingsPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let foreground = theme.foreground;
        let background = theme.background;
        div()
            .size_full()
            .flex()
            .bg(background)
            .text_color(foreground)
            .child(
                div()
                    .w(px(172.))
                    .h_full()
                    .flex_shrink_0()
                    .p(px(22.))
                    .border_r_1()
                    .border_color(border)
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Next Terminal"),
                    )
                    .child(div().text_size(px(12.)).text_color(muted).child("设置样板"))
                    .child(div().h(px(24.)))
                    .child(
                        div()
                            .rounded_md()
                            .px(px(12.))
                            .py(px(9.))
                            .bg(theme.secondary)
                            .child("外观"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(muted)
                            .child("独立运行 · 不修改终端"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .p(px(30.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_start()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(7.))
                                    .child(
                                        div()
                                            .text_size(px(26.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("让终端适合你"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(muted)
                                            .child("调整字体、字号和主题，即刻查看效果。"),
                                    ),
                            )
                            .child(Button::new("reset").label("恢复默认").on_click(
                                cx.listener(|this, _, window, cx| this.reset(window, cx)),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .gap(px(30.))
                            .child(
                                div()
                                    .w(px(260.))
                                    .flex_shrink_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("字体"),
                                    )
                                    .child(
                                        div().h(px(36.)).w_full().flex_shrink_0().child(
                                            Select::new(&self.font_select)
                                                .accessibility_label("终端字体")
                                                .w_full(),
                                        ),
                                    )
                                    .child(div().text_size(px(11.)).text_color(muted).child(
                                        format!("本机 {} 个字体族，可输入搜索。", self.font_count),
                                    ))
                                    .child(div().h(px(14.)))
                                    .child(
                                        div()
                                            .flex()
                                            .justify_between()
                                            .items_center()
                                            .child("字号")
                                            .child(format!("{} px", self.font_size as u32)),
                                    )
                                    .child(
                                        div().h(px(28.)).w_full().flex_shrink_0().child(
                                            Slider::new(&self.size_slider)
                                                .accessibility_label("终端字号")
                                                .w_full(),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .justify_between()
                                            .items_center()
                                            .child(Button::new("smaller").label("−").on_click(
                                                cx.listener(|this, _, window, cx| {
                                                    this.adjust_size(-1., window, cx)
                                                }),
                                            ))
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(muted)
                                                    .child("12 — 28 px"),
                                            )
                                            .child(Button::new("larger").label("+").on_click(
                                                cx.listener(|this, _, window, cx| {
                                                    this.adjust_size(1., window, cx)
                                                }),
                                            )),
                                    )
                                    .child(div().h(px(14.)))
                                    .child(div().font_weight(FontWeight::MEDIUM).child("主题"))
                                    .child(
                                        div().h(px(36.)).w_full().flex_shrink_0().child(
                                            Select::new(&self.theme_select)
                                                .accessibility_label("终端主题")
                                                .w_full(),
                                        ),
                                    )
                                    .child(div().flex_1())
                                    .child(div().text_size(px(11.)).text_color(muted).child(
                                        "固定样例预览，不启动 shell。更改仅在本次窗口内生效。",
                                    )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .child(div().text_size(px(12.)).text_color(muted).child(
                                        format!(
                                            "实时预览 · {} · {} px",
                                            self.font_family, self.font_size as u32
                                        ),
                                    ))
                                    .child(div().flex_1().min_h_0().child(self.preview())),
                            ),
                    ),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-w", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            Theme::change(ThemeMode::Dark, None, cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1120.), px(730.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(960.), px(560.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Next Terminal · 外观样板".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| SettingsPreview::new(window, cx)),
            )
            .expect("open settings preview");
            cx.activate(true);
        });
}
