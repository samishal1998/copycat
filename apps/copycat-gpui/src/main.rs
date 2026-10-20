//! Copycat desktop, drawn with GPUI. A client of the daemon like every other:
//! it owns no clipboard state, it asks `copycatd` over the socket and draws the
//! answer.

mod app;
mod screens;

use gpui_kit::component::Theme;
use gpui_kit::*;

fn main() {
    gpui_kit::application()
        // Without an asset source every icon renders blank.
        .with_assets(gpui_kit::assets::Assets)
        // macOS keeps an app alive after its last window by default; this one
        // has no tray yet, so a closed window would leave nothing to reopen.
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Copycat".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::centered(size(px(900.), px(620.)), cx)),
                window_min_size: Some(size(px(640.), px(420.))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                // The component theme starts Light whatever the OS says.
                Theme::sync_system_appearance(Some(window), cx);
                cx.new(|cx| app::AppView::new(window, cx))
            })
            .expect("failed to open the Copycat window");
            cx.activate(true);
        });
}
