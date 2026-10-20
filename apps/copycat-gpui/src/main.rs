//! Copycat desktop, drawn with GPUI. A client of the daemon like every other:
//! it owns no clipboard state, it asks `copycatd` over the socket and draws the
//! answer.

mod app;
mod bindings;
mod brand;
#[cfg(target_os = "macos")]
mod dropdown;
#[cfg(target_os = "macos")]
mod floating;
#[cfg(target_os = "macos")]
mod overlay;
mod screens;
mod settings;
#[cfg(target_os = "macos")]
mod tray;

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
        brand::init(cx);
        settings::Settings::start(cx);
        app::Daemon::start(cx);
        let main = open_main(cx);
        #[cfg(target_os = "macos")]
        {
            tray::install(main, cx);
            floating::start(cx);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = main;
        cx.activate(true);
        // Screenshot support: CI opens the dropdown at a fixed spot, once the
        // main window has settled, so its activation cannot blur the panel shut.
        #[cfg(target_os = "macos")]
        if std::env::var_os("COPYCAT_GPUI_DROPDOWN").is_some() {
            cx.spawn(async |cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                cx.update(|cx| tray::open(None, point(px(40.), px(40.)), cx));
            })
            .detach();
        }
    });
}

/// Open the main window; the tray keeps the handle to bring it back.
fn open_main(cx: &mut App) -> AnyWindowHandle {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Copycat".into()),
            ..Default::default()
        }),
        // The Tauri main window's size.
        // Screenshot runs use a taller window, so a whole screen fits in a shot.
        window_bounds: Some(WindowBounds::centered(
            size(px(940.), px(if std::env::var_os("COPYCAT_GPUI_SCREEN").is_some() { 980. } else { 640. })),
            cx,
        )),
        window_min_size: Some(size(px(720.), px(480.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| app::AppView::new(window, cx)))
    .expect("failed to open the Copycat window")
    .0
}
