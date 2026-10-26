# GPUI floating button: rendering glitches and fixes

Seen with gpui-kit 0.8.0 (gpui-pre 0.3.9) on a small always-on-top window: a round button that
grows into a panel when clicked, moved and resized by native calls (`NSWindow.setFrame` on macOS,
x11rb `configure_window` on X11). There are four separate problems, and they look alike.

## 1. Opaque square behind the round button

**Symptom:** a white or dark square shows behind the circle and behind the panel's rounded
corners, even though the window is created with `WindowBackgroundAppearance::Transparent`.

**Cause:** `gpui_kit::open_window` wraps the view in `Root`, and `Root` paints the theme
background over the whole window. That opaque fill covers the transparent window.

**Fix:** open the window yourself and give `Root` a transparent background:

```rust
let opened = cx.open_window(options, |window, cx| {
    let view = cx.new(|cx| Floating::new(window, cx));
    cx.new(|cx| gpui_kit::base::Root::new(view, window, cx).bg(transparent_black()))
});
```

Draw the shadow in GPUI (`.shadow(...)` on the button or panel), and turn off the macOS window
shadow (`ns_window.setHasShadow(false)`). Otherwise AppKit outlines the whole rectangle.

## 2. Panel draws late, clipped, or at the old size (macOS)

**Symptom:** after a click, the window resizes but the content keeps laying out at the button's
size. The panel appears clipped or a beat late, and only catches up when something else (a hover
or a poll) redraws it.

**Cause:** the resize happens inside a view update (`setFrame` is called from a click handler).
AppKit calls GPUI's resize callback synchronously. GPUI can't borrow the app while the update
holds it, so it drops the callback, and the window's viewport stays at the old size.

**Fix:** after any programmatic move or resize, tell GPUI the bounds changed, then notify. Don't
let AppKit draw synchronously either:

```rust
// display: NO. A synchronous display here draws inside GPUI's borrow and does nothing.
ns.setFrame_display(rect, false);
```

```rust
native::set_frame(window, x, y, w, h);
window.defer(cx, |window, cx| window.bounds_changed(cx)); // deferred: see 3
cx.notify();
```

## 3. Panic when opening the panel: "cannot update X while it is already being updated"

**Symptom:** a crash on macOS when the panel opens, right after adding the fix from section 2.

**Cause:** calling `window.bounds_changed(cx)` directly inside the view's update fires the
window's bounds observers at once. If one of them updates the same view (for example a move
observer that saves the button's position), GPUI tries to update an entity that is already being
updated, and panics.

**Fix:** call it with `window.defer(...)`, as in the snippet above. It then runs after the
current update ends. To reproduce on Linux, register the same bounds observer there.

## 4. Sluggish animation and hover (about 30 fps)

**Symptom:** hover, press and grow feel slower on the floating window than in the main window.

**Cause:** the popup is never the key window (`focus: false`, a non-activating panel), so GPUI
treats it as inactive and limits it to its inactive frame rate, about 30 fps.

**Fix:** in its `WindowOptions`:

```rust
WindowOptions {
    kind: WindowKind::PopUp,                 // X11: override-redirect; macOS: non-activating panel
    window_background: WindowBackgroundAppearance::Transparent,
    focus: false,
    inactive_frame_interval: None,           // no 30 fps cap for a window that is never key
    ..Default::default()
}
```

## Related: the floating window steals keyboard focus (macOS)

GPUI's panel is non-activating, but it still answers yes to `canBecomeKeyWindow`, so a click made
it the key window. The user's typing then went to the app instead of where they were typing. Fix:

```rust
if let Ok(panel) = ns.clone().downcast::<NSPanel>() {
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(false);
}
ns.setCollectionBehavior(CanJoinAllSpaces | FullScreenAuxiliary | Stationary);
```

## Checklist

- [ ] The floating window's `Root` uses `.bg(transparent_black())`, not `gpui_kit::open_window`.
- [ ] `setHasShadow(false)`, with the shadow drawn in GPUI.
- [ ] `setFrame_display(_, false)`, followed by `window.defer(cx, |w, cx| w.bounds_changed(cx))` and `cx.notify()`.
- [ ] `inactive_frame_interval: None` on the popup.
- [ ] `setBecomesKeyOnlyIfNeeded(true)` on the `NSPanel`.
