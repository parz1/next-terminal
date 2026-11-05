use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    size,
};

mod chrome;
mod editor;
mod grid;
mod ime;
mod keys;
mod theme;
mod view;

use theme::Theme;
use view::TerminalWindow;

fn main() {
    // run 启动原生应用事件循环；窗口事件和绘制由 GPUI 调度。
    Application::new().run(|cx: &mut App| {
        cx.set_global(Theme::graphite());
        editor::bind_keys(cx);
        // macOS 关闭最后一个窗口通常不会退出应用，这里显式结束进程。
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(920.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.0), px(320.0))),
                // 透明标题栏让终端背景延伸到窗口顶部；红绿灯按钮与自绘标题垂直居中。
                titlebar: Some(TitlebarOptions {
                    title: Some("Next Terminal".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(13.0), px(11.0))),
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TerminalWindow::new(window, cx)),
        )
        .expect("无法创建 GPUI 窗口");

        cx.activate(true);
    });
}
