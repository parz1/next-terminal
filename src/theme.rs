//! 界面设计令牌：颜色、字体与尺寸的唯一来源。视图、命令框和网格绘制都从这里取值，
//! 不在组件里散落十六进制颜色或像素常量。以后接入设置页或浅色主题时只替换 Theme。
use gpui::{App, Global, Hsla, Rgba, rgb, rgba};

/// 终端网格颜色使用 0xRRGGBB，便于纯函数测试；只在绘制时转换为 GPUI 颜色。
pub type Rgb = u32;

#[derive(Clone, Debug)]
pub struct Theme {
    pub mono_family: &'static str,
    pub ui_family: &'static str,
    pub metrics: Metrics,
    pub colors: Colors,
    pub ansi: [Rgb; 16],
}

#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub font_size: f32,
    pub line_height: f32,
    /// 终端网格左右留白；上下留白由标题栏与底部区域负责。
    pub grid_padding_x: f32,
    pub grid_padding_y: f32,
    pub title_height: f32,
    /// macOS 透明标题栏左侧为红绿灯按钮预留的宽度。
    pub title_inset_left: f32,
    pub status_height: f32,
    pub composer_height: f32,
    pub composer_margin: f32,
    pub radius: f32,
    pub ui_text: f32,
    pub ui_small: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub background: Rgb,
    pub foreground: Rgb,
    pub surface: Rgb,
    pub surface_raised: Rgb,
    pub border: Rgb,
    pub border_strong: Rgb,
    pub text: Rgb,
    pub text_muted: Rgb,
    pub text_faint: Rgb,
    pub accent: Rgb,
    pub selection: Rgb,
    pub cursor: Rgb,
    pub cursor_text: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
}

impl Global for Theme {}

impl Theme {
    pub fn global(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    /// 暖石墨深色主题。琥珀色强调沿用项目原有识别色，其余颜色降低饱和度，
    /// 让终端输出而不是界面边框成为视觉重心。
    pub fn graphite() -> Self {
        Self {
            mono_family: MONO_FAMILY,
            ui_family: UI_FAMILY,
            metrics: Metrics {
                font_size: 14.0,
                line_height: 20.0,
                grid_padding_x: 14.0,
                grid_padding_y: 6.0,
                title_height: 34.0,
                title_inset_left: if cfg!(target_os = "macos") {
                    76.0
                } else {
                    14.0
                },
                status_height: 26.0,
                composer_height: 78.0,
                composer_margin: 10.0,
                radius: 10.0,
                ui_text: 12.5,
                ui_small: 11.0,
            },
            colors: Colors {
                background: 0x1b1a19,
                foreground: 0xe2ded6,
                surface: 0x242321,
                surface_raised: 0x2d2b28,
                border: 0x302e2b,
                border_strong: 0x46433d,
                text: 0xe2ded6,
                text_muted: 0x9a958b,
                text_faint: 0x6e6a62,
                accent: 0xe6b75d,
                selection: 0x3a4a5d,
                cursor: 0xe6b75d,
                cursor_text: 0x1b1a19,
                success: 0x98bf7a,
                warning: 0xe6b75d,
                danger: 0xe06c62,
            },
            ansi: [
                0x2b2927, 0xe06c62, 0x98bf7a, 0xe6b75d, 0x6fa3d8, 0xc58fcf, 0x6cbfb6, 0xcfcac0,
                0x6e6a62, 0xf08a80, 0xb3d696, 0xf2cd86, 0x95bdea, 0xdaade1, 0x90d6cd, 0xf5f2eb,
            ],
        }
    }

    /// Alacritty 颜色索引：0–15 ANSI、16–231 色立方、232–255 灰阶，
    /// 256 起为前景、背景、光标及暗色变体（见 vte NamedColor）。
    pub fn indexed(&self, index: usize) -> Rgb {
        match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let value = index - 16;
                let level = |x: usize| if x == 0 { 0 } else { 55 + 40 * x as u32 };
                level(value / 36) << 16 | level((value / 6) % 6) << 8 | level(value % 6)
            }
            232..=255 => {
                let value = 8 + 10 * (index - 232) as u32;
                value << 16 | value << 8 | value
            }
            257 => self.colors.background,
            258 => self.colors.cursor,
            // DimBlack..DimWhite：向背景混合，而不是简单减半，避免暗色发脏。
            259..=266 => mix(self.ansi[index - 259], self.colors.background, 0.66),
            268 => mix(self.colors.foreground, self.colors.background, 0.66),
            _ => self.colors.foreground,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::graphite()
    }
}

#[cfg(target_os = "macos")]
const MONO_FAMILY: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
const MONO_FAMILY: &str = "DejaVu Sans Mono";

// 界面文字用系统字体，中文自动回退到苹方，比等宽字体里的中文回退更整齐。
#[cfg(target_os = "macos")]
const UI_FAMILY: &str = ".SystemUIFont";
#[cfg(not(target_os = "macos"))]
const UI_FAMILY: &str = "Noto Sans CJK SC";

/// 按比例混合两种颜色；`amount` 为前者所占比例。
pub fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    let channel = |shift: u32| {
        let x = ((a >> shift) & 0xff) as f32;
        let y = ((b >> shift) & 0xff) as f32;
        ((x * amount + y * (1.0 - amount)).round() as u32).min(255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

pub fn color(value: Rgb) -> Hsla {
    rgb(value).into()
}

/// 半透明强调色，例如命令框聚焦描边和按钮底色。
pub fn tint(value: Rgb, alpha: f32) -> Rgba {
    rgba(value << 8 | (alpha.clamp(0.0, 1.0) * 255.0).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_covers_named_and_cube_indices() {
        let theme = Theme::graphite();
        assert_eq!(theme.indexed(1), theme.ansi[1]);
        assert_eq!(theme.indexed(16), 0x000000);
        assert_eq!(theme.indexed(231), 0xffffff);
        assert_eq!(theme.indexed(232), 0x080808);
        assert_eq!(theme.indexed(256), theme.colors.foreground);
        assert_eq!(theme.indexed(257), theme.colors.background);
        assert_eq!(theme.indexed(258), theme.colors.cursor);
        assert_eq!(theme.indexed(267), theme.colors.foreground);
        // 暗色在原色与背景之间。
        let dim_red = theme.indexed(260);
        assert!(dim_red >> 16 < theme.ansi[1] >> 16);
        assert!(dim_red >> 16 > theme.colors.background >> 16);
    }

    #[test]
    fn mix_and_tint_keep_channels_separate() {
        assert_eq!(mix(0xff0000, 0x0000ff, 1.0), 0xff0000);
        assert_eq!(mix(0xff0000, 0x0000ff, 0.0), 0x0000ff);
        assert_eq!(mix(0xffffff, 0x000000, 0.5), 0x808080);
        let tinted = tint(0xe6b75d, 0.5);
        assert!((tinted.a - 128.0 / 255.0).abs() < 1e-6);
        assert!((tinted.r - 0xe6 as f32 / 255.0).abs() < 1e-6);
    }
}
