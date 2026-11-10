//! The menu-bar icon (macOS only): left click toggles the dropdown, right
//! click drops a menu with the two escape hatches that must always work.
//!
//! tray-icon calls its handlers on the main thread from AppKit, but they must
//! be `Send + Sync` and get no `App`, so they only post to a channel; a GPUI
//! task drains it and does the work with a real `&mut App`.

use std::time::{Duration, Instant};

use gpui_kit::*;
use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSNumber, NSString};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::dropdown::Dropdown;

/// The Tauri panel's size.
const PANEL: Size<Pixels> = Size { width: px(392.), height: px(588.) };

enum TrayMsg {
    Toggle,
    Menu(MenuEvent),
}

struct Tray {
    /// Dropping it removes the icon.
    icon: TrayIcon,
    main: AnyWindowHandle,
    panel: Option<AnyWindowHandle>,
    /// When blur last closed the panel: the click on the icon that caused the
    /// blur must not reopen it.
    blurred_at: Option<Instant>,
}

impl Global for Tray {}

pub fn install(main: AnyWindowHandle, cx: &mut App) {
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id("open", "Open Copycat", true, None),
        &MenuItem::with_id("quit", "Quit Copycat", true, None),
    ])
    .expect("tray menu");
    // A template image, the brand mark in black and alpha, which the system
    // tints for a light or dark menu bar like every built-in menu extra.
    let icon = TrayIconBuilder::new()
        .with_icon(template_icon())
        .with_icon_as_template(true)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Copycat")
        .build()
        .expect("tray icon");

    let (tx, rx) = async_channel::unbounded();
    let icon_tx = tx.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        // On the button's release, as the Tauri app does: a click sends both.
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = icon_tx.try_send(TrayMsg::Toggle);
        }
    }));
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = tx.try_send(TrayMsg::Menu(event));
    }));

    cx.set_global(Tray { icon, main, panel: None, blurred_at: None });
    cx.spawn(async move |cx| {
        while let Ok(msg) = rx.recv().await {
            cx.update(|cx| match msg {
                TrayMsg::Toggle => toggle(cx),
                TrayMsg::Menu(event) if event.id == "open" => show_main(cx),
                // GPUI posts `terminate:`, so this never re-enters the app.
                TrayMsg::Menu(event) if event.id == "quit" => cx.quit(),
                TrayMsg::Menu(_) => {}
            });
        }
    })
    .detach();
}

fn template_icon() -> Icon {
    let bytes = include_bytes!("../assets/tray-template.png");
    let mut reader = png::Decoder::new(std::io::Cursor::new(&bytes[..]))
        .read_info()
        .expect("tray icon png");
    let mut rgba = vec![0; reader.output_buffer_size().expect("tray icon size")];
    let info = reader.next_frame(&mut rgba).expect("tray icon pixels");
    // `Icon::from_rgba` takes the bytes on trust.
    assert_eq!((info.color_type, info.bit_depth), (png::ColorType::Rgba, png::BitDepth::Eight));
    rgba.truncate(info.buffer_size());
    Icon::from_rgba(rgba, info.width, info.height).expect("tray icon rgba")
}

/// Open the dropdown under the icon, or close it if it is open.
///
/// A PopUp window is GPUI's own non-activating NSPanel at pop-up level that
/// joins every Space, fullscreen ones included: what the Tauri app builds by
/// hand, with no class swap. It becomes key without activating Copycat, so the
/// app the user is in stays frontmost.
// ponytail: if a Mac shows the panel pulling the user off a fullscreen Space,
// do what the Tauri app does: the Accessory activation policy at launch, and
// Regular in `show_main`.
fn toggle(cx: &mut App) {
    // A handle can outlive its panel; then this click opens a new one.
    if let Some(panel) = cx.global_mut::<Tray>().panel.take()
        && panel.update(cx, |_, window, _| window.remove_window()).is_ok()
    {
        return;
    }
    let tray = cx.global::<Tray>();
    // Clicking the icon may take key status from the panel before the
    // release arrives; that blur already closed it, so this click must not
    // reopen it. ponytail: fixed debounce for that race.
    if tray.blurred_at.is_some_and(|t| t.elapsed() < Duration::from_millis(300)) {
        return;
    }
    let Some((display, origin)) = under_icon(&tray.icon) else { return };
    open(Some(display), origin, cx);
}

/// Open the dropdown at `origin` on `display` (the main one when `None`).
/// Screenshot support calls it directly, with no icon to drop it under.
pub fn open(display: Option<DisplayId>, origin: Point<Pixels>, cx: &mut App) {
    // `titlebar: None` still leaves the window Titled (GPUI needs that), but
    // with no close, minimize or resize bits, so AppKit draws no buttons.
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, PANEL))),
        display_id: display,
        titlebar: None,
        kind: WindowKind::PopUp,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Dropdown::new(window, cx)));
    if let Ok((handle, _)) = opened {
        cx.global_mut::<Tray>().panel = Some(handle);
    }
}

/// Close the dropdown from inside it. `blurred` when it lost focus, which
/// starts the reopen debounce in `toggle`.
pub fn close(window: &mut Window, blurred: bool, cx: &mut App) {
    window.remove_window();
    let tray = cx.global_mut::<Tray>();
    tray.panel = None;
    if blurred {
        tray.blurred_at = Some(Instant::now());
    }
}

/// Hand focus back to the app the user was in, before the daemon pastes, as
/// the Tauri app's `hand_focus_back` does. A no-op when Copycat isn't active.
pub fn deactivate() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).deactivate();
    }
}

/// The panel's origin in GPUI's terms: points from the top-left of the screen
/// the icon is on. tray-icon's own click rect is points times that screen's
/// backing scale, flipped against the main display, so it is read off the
/// status item instead, which is plain points and names its own screen.
fn under_icon(icon: &TrayIcon) -> Option<(DisplayId, Point<Pixels>)> {
    let mtm = MainThreadMarker::new()?;
    let window = icon.ns_status_item()?.button(mtm)?.window()?;
    let icon_frame = window.frame(); // Cocoa global points, y up
    let screen = window.screen()?;
    let frame = screen.frame();
    // The display id the way GPUI reads it; `CGDirectDisplayID()` on
    // NSScreen is too new an API to rely on.
    let number = screen.deviceDescription().objectForKey(&NSString::from_str("NSScreenNumber"))?;
    let id = number.downcast_ref::<NSNumber>()?.unsignedIntValue();
    let width = PANEL.width.as_f32() as f64;
    // Centred under the icon, kept on its screen.
    let x = icon_frame.origin.x + icon_frame.size.width / 2.0 - width / 2.0 - frame.origin.x;
    let x = x.clamp(0.0, (frame.size.width - width).max(0.0));
    // The icon's bottom edge, measured down from the screen's top.
    let y = frame.origin.y + frame.size.height - icon_frame.origin.y;
    Some((DisplayId::new(id as u64), point(px(x as f32), px(y as f32))))
}

/// Focus the main window, or open a new one if it was closed.
pub fn show_main(cx: &mut App) {
    let main = cx.try_global::<Tray>().map(|tray| tray.main);
    let alive = main.is_some_and(|handle| handle.update(cx, |_, window, _| window.activate_window()).is_ok());
    if !alive {
        let main = crate::open_main(cx);
        if cx.has_global::<Tray>() {
            cx.global_mut::<Tray>().main = main;
        }
    }
    cx.activate(true);
}
