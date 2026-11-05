use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

mod editor;
mod view;

use view::TerminalWindow;

fn main() {
    // run 启动原生应用事件循环；窗口事件和绘制由 GPUI 调度。
    Application::new().run(|cx: &mut App| {
        editor::bind_keys(cx);
        // macOS 关闭最后一个窗口通常不会退出应用，这里显式结束进程。
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(900.0), px(680.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(400.0), px(300.0))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Next Terminal".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TerminalWindow::new(window, cx)),
        )
        .expect("无法创建 GPUI 窗口");

        cx.activate(true);
    });
}
