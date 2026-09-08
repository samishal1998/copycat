// The full window. Same one-command bridge as the panel; four screens, each a
// thin view over daemon actions.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);

const SIZE_KEY = "menubar.pageSize"; // shared with the panel (same origin)

// Daemon actions a key can be bound to, with a hint of what they need.
const ACTIONS = [
  ["paste.mode", "pop / advance / paste the group — whatever the chord means now"],
  ["paste.next", "consume the session's next item"],
  ["paste.latest", "paste the newest clip"],
  ["paste.offset", 'paste an older clip — needs {"offset":1}'],
  ["stack.start", 'begin a stack — optional {"duplicates":"preserve"}'],
  ["queue.start", 'snapshot the newest N — needs {"last":5}'],
  ["queue.capture", "collect a queue from now on"],
  ["queue.seal", "seal the queue"],
  ["group.capture", 'collect a group — optional {"delimiter":", "}'],
  ["group.paste", "paste the captured group"],
  ["group.paste_last", 'join the newest N — needs {"last":3}'],
  ["session.stop", "end the active session"],
  ["session.reset", "return the cursor to the start"],
  ["history.pause", "stop recording copies"],
  ["history.resume", "resume recording copies"],
  ["history.clear", "clear history"],
];
const TUI_ACTIONS = [
  "confirm", "paste_next", "delete", "pin", "search", "toggle_raw", "add", "edit",
  "test", "toggle_pause", "stack_start", "queue_capture", "queue_seal",
  "group_capture", "group_paste", "session_stop", "session_reset",
  "next_tab", "prev_tab", "down", "up", "refresh", "help", "quit",
];

let current = "history";
let editing = null; // { kind, identity } while editing a binding

async function daemon(action, args) {
  try { return await invoke("daemon", { action, args: args ?? null }); }
  catch (message) { toast(String(message)); return null; }
}
function toast(text) {
  const el = $("toast"); el.textContent = text; el.hidden = false;
  clearTimeout(toast._t); toast._t = setTimeout(() => { el.hidden = true; }, 2400);
}
function escapeHtml(t) { const d = document.createElement("div"); d.textContent = t ?? ""; return d.innerHTML; }
function truncate(t, n) { t = t ?? ""; return t.length > n ? t.slice(0, n) + "…" : t; }
function age(ms) {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`; if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`; return `${Math.floor(s / 86400)}d`;
}

// ---- routing -------------------------------------------------------------

function show(screen) {
  current = screen;
  document.querySelectorAll(".nav").forEach((n) => n.classList.toggle("on", n.dataset.screen === screen));
  document.querySelectorAll(".screen").forEach((s) => s.classList.toggle("on", s.id === `screen-${screen}`));
  if (screen === "history") loadHistory();
  if (screen === "bindings") loadBindings();
  if (screen === "settings") loadSettings();
}

// ---- live rail + session -------------------------------------------------

function onState(payload) {
  const dot = $("rail-dot"), conn = $("rail-conn");
  if (!payload.connected) {
    dot.classList.add("off"); conn.textContent = "daemon offline";
    return;
  }
  dot.classList.remove("off"); conn.textContent = "connected";
  const status = payload.status ?? {};
  const core = status.core ?? {};
  $("rail-key").textContent = `key: ${status.key_storage ?? "—"}`;
  $("nav-hist").textContent = core.hot_items != null ? core.hot_items : "";
  renderSession(core.session ?? null, core.paused);
  $("nav-sess").textContent = core.session ? core.session.remaining : "";
}

function renderSession(session, paused) {
  const title = $("sess-title"), sub = $("sess-sub"), live = $("sess-live"), detail = $("sess-detail");
  if (!session) {
    title.textContent = paused ? "Paused" : "Session";
    sub.textContent = paused
      ? "Capture is paused — copies are not being recorded."
      : "No mode active — the clipboard behaves normally.";
    live.hidden = true;
    return;
  }
  title.textContent = session.mode.charAt(0).toUpperCase() + session.mode.slice(1);
  sub.textContent = session.state === "capturing"
    ? "Capturing — copies are being collected. The first paste seals it."
    : "Traversing — your own paste chord advances the cursor.";
  live.hidden = false;
  const rows = [
    ["state", session.state],
    ["duplicates", session.duplicate_policy],
    ["size", session.size],
    ["cursor", `${session.cursor} / ${session.size}`],
    ["remaining", session.remaining],
  ];
  if (session.delimiter != null) rows.push(["delimiter", JSON.stringify(session.delimiter)]);
  detail.innerHTML = rows.map(([k, v]) =>
    `<div class="rowline"><span class="c"><span class="a">${k}</span>&nbsp;&nbsp;${escapeHtml(String(v))}</span></div>`
  ).join("");
}

// ---- history -------------------------------------------------------------

let searchTimer = null;
async function loadHistory() {
  const q = $("search").value.trim();
  const reply = q
    ? await daemon("history.search", { query: q, limit: 100 })
    : await daemon("history.list", { limit: 100, raw: false });
  if (reply && reply.type === "clips") renderHistory(reply.clips, reply.truncated);
}

function renderHistory(clips, truncated) {
  const list = $("history-list");
  if (!clips.length) { list.innerHTML = `<div class="empty">No clips.</div>`; return; }
  list.innerHTML = clips.map((c) => `
    <div class="rowline" data-id="${c.id}">
      <span class="c">${escapeHtml(c.preview) || "<em>(empty)</em>"}${
        c.duplicate_run > 1 ? ` <span class="muted">×${c.duplicate_run}</span>` : ""
      }</span>
      <span class="tools">
        <span class="muted" style="font-family:var(--mono);font-size:10.5px">${age(c.captured_at)}</span>
        <button class="btn small ghost" data-act="pin">${c.pinned ? "★" : "☆"}</button>
        <button class="btn small ghost" data-act="paste">Paste</button>
        <button class="btn small ghost danger" data-act="delete">✕</button>
      </span>
    </div>`).join("") + (truncated ? `<div class="empty">Search stopped at the scan limit.</div>` : "");

  list.querySelectorAll(".rowline").forEach((row) => {
    const id = Number(row.dataset.id);
    const clip = clips.find((c) => c.id === id);
    row.querySelector('[data-act="paste"]').onclick = async () => {
      if (await daemon("paste.id", { id })) toast(`pasted ${truncate(clip.preview, 32)}`);
    };
    row.querySelector('[data-act="pin"]').onclick = async () => {
      if (await daemon("history.pin", { id, pinned: !clip.pinned })) loadHistory();
    };
    row.querySelector('[data-act="delete"]').onclick = async () => {
      if (await daemon("history.delete", { id })) loadHistory();
    };
  });
}

// ---- bindings ------------------------------------------------------------

async function loadBindings() {
  populateActions();
  const reply = await daemon("bind.list", {});
  if (!reply || reply.type !== "bindings") return;

  $("leader-trigger").value = reply.leader ?? "";
  $("leader-enabled").checked = reply.leader != null;
  $("nav-bind").textContent = reply.sequences.length + reply.hotkeys.length;

  const group = (title, items, kind, identityKey) => {
    if (!items.length) return "";
    return `<div style="margin:4px 0 2px" class="muted" >${title}</div>` + items.map((b) => `
      <div class="rowline">
        <span class="c"><b>${escapeHtml(b.trigger)}</b> <span class="a">→ ${escapeHtml(b.action)}</span>${
          b.args && Object.keys(b.args || {}).length ? ` <span class="a">${escapeHtml(JSON.stringify(b.args))}</span>` : ""
        }</span>
        <span class="tools">
          <button class="btn small ghost" data-edit='${encodeURIComponent(JSON.stringify({ kind, trigger: b.trigger, action: b.action, args: b.args }))}'>Edit</button>
          <button class="btn small ghost danger" data-remove='${encodeURIComponent(JSON.stringify({ kind, identity: b[identityKey] }))}'>Remove</button>
        </span>
      </div>`).join("");
  };

  let html = group("Leader sequences", reply.sequences, "leader", "trigger")
    + group("Hotkeys", reply.hotkeys, "hotkey", "trigger")
    + group("App keys (changed)", reply.tui, "tui", "action");

  if (reply.rejected.length) {
    html += `<div style="margin:10px 0 2px" class="muted">Not active</div>` + reply.rejected.map((r) =>
      `<div class="rowline"><span class="c"><b>${escapeHtml(r.trigger)}</b> <span class="a">${escapeHtml(r.reason)}</span></span><span class="pill warn">rejected</span></div>`
    ).join("");
  }
  $("bindings-list").innerHTML = html || `<div class="empty">No bindings configured.</div>`;

  $("bindings-list").querySelectorAll("[data-edit]").forEach((b) =>
    b.onclick = () => fillForm(JSON.parse(decodeURIComponent(b.dataset.edit))));
  $("bindings-list").querySelectorAll("[data-remove]").forEach((b) =>
    b.onclick = async () => {
      const { kind, identity } = JSON.parse(decodeURIComponent(b.dataset.remove));
      if (await daemon("bind.remove", { kind, trigger: identity })) loadBindings();
    });
}

function populateActions() {
  const kind = $("b-kind").value;
  const opts = kind === "tui" ? TUI_ACTIONS.map((a) => [a, ""]) : ACTIONS;
  const sel = $("b-action");
  const keep = sel.value;
  sel.innerHTML = opts.map(([name]) => `<option value="${name}">${name}</option>`).join("");
  if ([...sel.options].some((o) => o.value === keep)) sel.value = keep;
  updateArgHelp();
}
function updateArgHelp() {
  const found = ACTIONS.find((a) => a[0] === $("b-action").value);
  $("b-arg-help").textContent = found ? found[1] : "no arguments";
}

function fillForm(b) {
  editing = { kind: b.kind, identity: b.kind === "tui" ? b.action : b.trigger };
  $("b-kind").value = b.kind;
  populateActions();
  $("b-trigger").value = b.trigger;
  $("b-action").value = b.action;
  $("b-args").value = b.args && Object.keys(b.args || {}).length ? JSON.stringify(b.args) : "";
  $("edit-title").textContent = "Edit binding";
  $("b-save").textContent = "Save binding";
  $("b-cancel").hidden = false;
  updateArgHelp();
}
function resetForm() {
  editing = null;
  $("b-trigger").value = ""; $("b-args").value = "";
  $("edit-title").textContent = "Add a binding";
  $("b-save").textContent = "Add binding";
  $("b-cancel").hidden = true;
}

async function saveBinding() {
  const kind = $("b-kind").value;
  const trigger = $("b-trigger").value.trim();
  const action = $("b-action").value;
  if (!trigger) { toast("a trigger is required"); return; }
  let args = null;
  const raw = $("b-args").value.trim();
  if (raw && kind !== "tui") {
    try { args = JSON.parse(raw); } catch (e) { toast(`arguments are not valid JSON: ${e}`); return; }
  }
  // A renamed trigger removes the old entry first, so an edit can't leave two.
  if (editing) {
    const identity = kind === "tui" ? action : trigger;
    if (editing.kind !== kind || editing.identity !== identity) {
      await daemon("bind.remove", { kind: editing.kind, trigger: editing.identity });
    }
  }
  if (await daemon("bind.set", { kind, trigger, action, args })) {
    toast("binding saved");
    resetForm();
    loadBindings();
  }
}

// ---- settings ------------------------------------------------------------

function renderSize() { $("mb-size").textContent = String(parseInt(localStorage.getItem(SIZE_KEY), 10) || 6); }
function bumpSize(d) {
  const n = Math.min(24, Math.max(3, (parseInt(localStorage.getItem(SIZE_KEY), 10) || 6) + d));
  localStorage.setItem(SIZE_KEY, String(n));
  renderSize();
}

async function loadSettings() {
  renderSize();
  const cfg = await daemon("config.show", {});
  if (cfg && cfg.type === "config") $("cfg-toml").textContent = cfg.toml;

  const doc = await daemon("doctor", {});
  if (doc && doc.type === "doctor") {
    const cls = (s) => (s === "ok" ? "ok" : s === "degraded" ? "warn" : "bad");
    $("diag").innerHTML =
      `<div class="check"><span></span><span class="name">platform</span><span class="detail">${escapeHtml(doc.display_server)} — ${escapeHtml(doc.platform_support)}</span></div>` +
      doc.checks.map((c) =>
        `<div class="check"><span class="pill ${cls(c.status)}">${c.status}</span><span class="name">${escapeHtml(c.name)}</span><span class="detail">${escapeHtml(c.detail)}</span></div>`
      ).join("");
    renderPermCallout(doc);
  }
}

// When the event tap is refused, it is almost always Input Monitoring not
// granted to Copycat itself — the app launches the daemon, so the permission is
// attributed to the app. Say so, and offer the exact panes plus a restart.
function renderPermCallout(doc) {
  const box = $("perm-callout");
  const blocked = doc.checks.some(
    (c) => c.status === "unavailable" &&
      (c.name === "paste-interception" || c.name === "global-hotkeys") &&
      /input monitoring|event tap/i.test(c.detail)
  );
  if (!blocked) { box.innerHTML = ""; return; }

  box.innerHTML = `
    <div class="card" style="border-color:rgba(228,103,43,.4)">
      <h2 style="color:var(--active)">Grant permission to Copycat</h2>
      <p class="muted" style="margin:0 0 12px">
        Copycat launches the daemon, so macOS attributes its keyboard access to
        <b style="color:var(--text)">Copycat</b> — not to a terminal. Add Copycat
        to both lists, then restart the daemon.
      </p>
      <div class="actions">
        <button class="btn primary" id="perm-listen">Open Input Monitoring</button>
        <button class="btn" id="perm-ax">Open Accessibility</button>
        <button class="btn ghost" id="perm-restart">Restart daemon</button>
      </div>
      <p class="field-help">Input Monitoring lets the daemon read your hotkeys and paste chord; Accessibility lets it paste.</p>
    </div>`;
  $("perm-listen").onclick = () => invoke("open_settings", { pane: "input-monitoring" });
  $("perm-ax").onclick = () => invoke("open_settings", { pane: "accessibility" });
  $("perm-restart").onclick = async () => {
    toast("restarting the daemon…");
    await invoke("restart_daemon");
    setTimeout(loadSettings, 1200);
  };
}

// ---- wire + boot ---------------------------------------------------------

document.querySelectorAll(".nav[data-screen]").forEach((n) => n.onclick = () => show(n.dataset.screen));

$("search").addEventListener("input", () => { clearTimeout(searchTimer); searchTimer = setTimeout(loadHistory, 200); });
$("clear-unpinned").onclick = async () => { if (await daemon("history.clear", { keep_pinned: true })) loadHistory(); };
$("clear-all").onclick = async () => { if (await daemon("history.clear", {})) loadHistory(); };

document.querySelectorAll("#screen-session [data-action]").forEach((b) =>
  b.onclick = async () => { if (await daemon(b.dataset.action, {})) toast("done"); });

$("leader-save").onclick = async () => {
  const trigger = $("leader-trigger").value.trim();
  const enabled = $("leader-enabled").checked;
  if (await daemon("bind.leader", { trigger: trigger || null, enabled })) { toast("leader saved"); loadBindings(); }
};
$("b-kind").onchange = () => populateActions();
$("b-action").onchange = updateArgHelp;
$("b-save").onclick = saveBinding;
$("b-cancel").onclick = resetForm;

$("cfg-reload").onclick = async () => { if (await daemon("bind.reload", {})) { toast("config reloaded"); loadSettings(); } };
$("mb-down").onclick = () => bumpSize(-1);
$("mb-up").onclick = () => bumpSize(1);

listen("daemon-state", (e) => onState(e.payload));
show("history");
