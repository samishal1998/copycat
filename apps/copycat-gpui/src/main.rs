//! Copycat desktop, drawn with GPUI. A client of the daemon like every other:
//! it owns no clipboard state, it asks `copycatd` over the socket and draws the
//! answer.

mod app;
#[cfg(target_os = "macos")]
mod dropdown;
mod screens;
#[cfg(target_os = "macos")]
mod tray;

use gpui_kit::component::Theme;
use gpui_kit::*;

fn main() {
    let app = gpui_kit::application()
        // Without an asset source every icon renders blank.
        .with_assets(gpui_kit::assets::Assets)
        // On macOS the tray keeps the app alive with no window open. Elsewhere
        // there is no tray yet, so a closed window would leave nothing to reopen.
        .with_quit_mode(if cfg!(target_os = "macos") {
            QuitMode::Explicit
        } else {
            QuitMode::LastWindowClosed
        });
    // A Dock click while no window is open brings the main window back.
    #[cfg(target_os = "macos")]
    app.on_reopen(tray::show_main);
    app.run(|cx| {
        gpui_kit::init(cx);
        app::Daemon::start(cx);
        let main = open_main(cx);
        #[cfg(target_os = "macos")]
        tray::install(main, cx);
        #[cfg(not(target_os = "macos"))]
        let _ = main;
        cx.activate(true);
    });
}

/// Open the main window; the tray keeps the handle to bring it back.
fn open_main(cx: &mut App) -> AnyWindowHandle {
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
    .expect("failed to open the Copycat window")
    .0
}
