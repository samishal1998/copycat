// A fake daemon rich enough to render every Tauri screen for reference shots.
(() => {
  const now = Date.now();
  const previews = ["git commit -m \"fix tray placement\"", "https://github.com/samishal1998/copycat", "SELECT * FROM clips WHERE pinned = 1;",
    "Meeting notes — Q4 roadmap: GPUI front end, signing, fullscreen panels", "cargo install --locked copycat", "samishal.1998@example.com",
    "fn main() { println!(\"hi\"); }", "0x0063_6f70_7963_6174", "Lorem ipsum dolor sit amet, consectetur adipiscing elit", "rc.17 works over fullscreen now"];
  const clips = previews.map((p, i) => ({ id: 100 - i, preview: p, pinned: i === 1 || i === 4, duplicate_run: i === 2 ? 3 : 1,
    captured_at: now - (i * 7 + 1) * 60000, bytes: p.length, media_type: "text/plain" }));
  const s = { id: 7, mode: "stack", state: "ready", duplicate_policy: "collapse", items: [100, 99, 98, 97], cursor: 1 };
  const summary = () => ({ id: s.id, mode: s.mode, state: s.state, duplicate_policy: s.duplicate_policy, size: s.items.length,
    cursor: s.cursor, remaining: s.items.length - s.cursor, next: s.items[s.cursor] ?? null, delimiter: null, created_at: 0 });
  const status = () => ({ type: "status", protocol_version: 1, daemon_version: "0.6.0-rc.3", uptime_ms: 3600000, key_storage: "keyring",
    core: { paused: false, hot_items: 584, session: window.__noSession ? null : summary(), latest: null } });
  const doctor = { type: "doctor", daemon_version: "0.6.0-rc.3", platform: "macos-aarch64", display_server: "macos",
    platform_support: "experimental: written against the documented APIs but never run", capabilities: [],
    checks: [
      { name: "clipboard", status: "ok", detail: "arboard reads text/plain" },
      { name: "macos-permissions", status: window.__perm ? "unavailable" : "ok", detail: window.__perm ? "Accessibility is not granted to the app that started copycatd (Copycat, or your terminal)…" : "accessibility: true, post events: true, input monitoring: false" },
      { name: "repeat-copy-detection", status: "degraded", detail: "this backend exposes no change counter" },
      { name: "paste-injection", status: "ok", detail: "macos-cgevent" },
      { name: "key-storage", status: "ok", detail: "the OS keyring holds the key" },
      { name: "history-store", status: "ok", detail: "584 clips persisted" } ] };
  const binds = { type: "bindings", leader: { trigger: "ctrl+alt+space", enabled: true, timeout_ms: 1200 }, bindings: [
    { kind: "leader", trigger: "s", action: "stack.start", args: null }, { kind: "leader", trigger: "q", action: "queue.capture", args: null },
    { kind: "hotkey", trigger: "ctrl+alt+v", action: "paste.mode", args: null } ] };
  async function invoke(cmd, a) {
    if (cmd !== "daemon") return null;
    const { action } = a;
    if (action === "history.list" || action === "history.search") return { type: "clips", truncated: false, clips };
    if (action === "session.items") return { type: "clips", truncated: false, clips: s.items.map((id) => clips.find((c) => c.id === id)) };
    if (action === "status") return status();
    if (action === "doctor") return doctor;
    if (action === "bind.list") return binds;
    if (action === "config.show") return { type: "config", toml: "[history]\ncapture_html = true\n\n[leader]\ntrigger = \"ctrl+alt+space\"\n" };
    return { type: "done" };
  }
  const listeners = [];
  setInterval(() => listeners.forEach((cb) => cb({ payload: { connected: true, installed: true, status: status() } })), 300);
  window.__TAURI__ = { core: { invoke }, event: { listen: (_e, cb) => { listeners.push(cb); return Promise.resolve(() => {}); } } };
})();
