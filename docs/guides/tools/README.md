# Helper tools for agents that cannot see a screen

| File | Use |
|---|---|
| `mac-check.sh` | macOS type-check of any crate from Linux (stub cc/ar), `-D warnings` |
| `mac-check-gpui.sh` | same for a GPUI app (stubs gpui-pre-apple's Metal build script) |
| `fake-cc.sh`, `fake-ar.sh` | the stub compiler and archiver |
| `mock-tauri.js` | fake `window.__TAURI__` + daemon with sample clips/session/doctor/bindings |
| `ref-shots.js` | Playwright script rendering every Tauri screen to PNG (the reference for GPUI work) |

Nothing here links or runs macOS code. A clean check means "type-checks", not "works".
