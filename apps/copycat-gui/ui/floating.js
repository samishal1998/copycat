// The floating button and the panel it opens into. One window, two sizes:
// collapsed it is a round button that sits above every app; clicked, it grows
// into a panel with the mode, the items waiting to paste (reorderable), and the
// session controls. Drag the button — or the panel's header — to move it.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);

async function daemon(action, args) {
  try { return await invoke("daemon", { action, args: args ?? null }); }
  catch (message) { say(String(message)); return null; }
}
function say(text) {
  $("msg").textContent = text;
  clearTimeout(say._t);
  say._t = setTimeout(() => { $("msg").textContent = ""; }, 3200);
}
function truncate(t, n) { t = t ?? ""; return t.length > n ? t.slice(0, n) + "…" : t; }

const list = SessionList($("items"), daemon);
let expanded = false;
let last = { connected: false, core: {}, session: null };

// ---- the two states -------------------------------------------------------

async function setExpanded(on) {
  if (on === expanded) return;
  expanded = on;
  // Grow the window before drawing the panel into it; hide the panel before
  // shrinking, so neither state is ever drawn at the other's size.
  if (on) {
    await invoke("floating_expand", { expanded: true });
    document.body.className = "expanded";
    render();
  } else {
    document.body.className = "collapsed";
    await invoke("floating_expand", { expanded: false });
  }
}

// ---- live state -----------------------------------------------------------

listen("daemon-state", ({ payload }) => {
  const core = payload.connected ? (payload.status?.core ?? {}) : {};
  last = { connected: payload.connected, core, session: core.session ?? null };
  renderBubble();
  if (expanded) render();
});

function renderBubble() {
  const { connected, core, session: s } = last;
  const b = $("bubble"), badge = $("bubble-badge");
  b.classList.toggle("off", !connected);
  b.dataset.mode = s ? s.mode : core.paused ? "paused" : "";
  badge.hidden = !s;
  // The mode's letter plus what is left (or collected), so the badge reads
  // without the ring's colour: S3 = a stack with three to go.
  if (s) badge.textContent = s.mode[0].toUpperCase() + (s.state === "capturing" ? s.size : s.remaining);
  const label = !connected ? "Copycat — daemon offline. Open controls"
    : s ? `Copycat — ${s.mode}, ${s.state === "capturing" ? `capturing, ${s.size} collected` : `${s.remaining} left`}. Open controls`
    : core.paused ? "Copycat — capture paused. Open controls"
    : "Open Copycat controls";
  b.setAttribute("aria-label", label);
  b.title = label;
}

function render() {
  const { connected, core, session: s } = last;
  $("dot").classList.toggle("off", !connected);

  if (!connected) {
    $("mode").textContent = "OFFLINE";
    $("size").textContent = "";
  } else if (s) {
    $("mode").textContent = s.mode.toUpperCase();
    $("size").textContent = s.state === "capturing"
      ? `capturing · ${s.size}`
      : `${s.cursor}/${s.size} · ${s.remaining} left`;
  } else {
    $("mode").textContent = core.paused ? "PAUSED" : "NORMAL";
    $("size").textContent = core.hot_items != null ? `${core.hot_items} clips` : "";
  }

  $("idle").hidden = !!s;
  $("items").hidden = !s;
  list.update(s);

  // Only offer what the current mode can do.
  const off = {
    "paste.mode": !s,
    "group.paste": !(s && s.mode === "group"),
    "queue.seal": !(s && s.mode === "queue" && s.state === "capturing"),
    "session.reset": !s,
    "session.stop": !s,
  };
  document.querySelectorAll(".act").forEach((b) => {
    b.disabled = !connected || Boolean(off[b.dataset.action]);
  });
}

// ---- actions --------------------------------------------------------------

document.querySelectorAll(".act").forEach((b) => b.addEventListener("click", async () => {
  const reply = await daemon(b.dataset.action, {});
  if (!reply) return;
  if (reply.type === "pasted") say(`pasted ${truncate(reply.preview, 30)}`);
  else if (reply.type === "session_started") say(`${b.textContent.toLowerCase()} started`);
  else say("done");
}));

$("collapse").addEventListener("click", () => setExpanded(false));
$("open-main").addEventListener("click", () => invoke("open_main"));

// ---- click vs drag --------------------------------------------------------
// A press that travels more than a few pixels moves the window; anything less
// is a click. The move is relayed as deltas to Rust, because the OS drag can
// only start from the mouse-down itself, before we know it is a drag.

function dragToMove(el, { onClick, skip } = {}) {
  let start = null, prev = null, dragged = false;
  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || (skip && skip(e))) return;
    start = prev = { x: e.screenX, y: e.screenY };
    dragged = false;
    el.setPointerCapture(e.pointerId);
  });
  el.addEventListener("pointermove", (e) => {
    if (!start) return;
    if (!dragged && Math.hypot(e.screenX - start.x, e.screenY - start.y) < 4) return;
    dragged = true;
    const dx = e.screenX - prev.x, dy = e.screenY - prev.y;
    prev = { x: e.screenX, y: e.screenY };
    if (dx || dy) invoke("move_window_by", { dx, dy });
  });
  const end = () => { start = null; };
  el.addEventListener("pointerup", end);
  el.addEventListener("pointercancel", end);
  // Activation stays on `click`, so the keyboard path (Enter/Space) still
  // works; a click that ends a drag is swallowed.
  if (onClick) {
    el.addEventListener("click", (e) => {
      if (dragged) { dragged = false; e.preventDefault(); return; }
      onClick();
    });
  }
}

dragToMove($("bubble"), { onClick: () => setExpanded(true) });
dragToMove($("grab"), { skip: (e) => e.target.closest("button") != null });

renderBubble();
