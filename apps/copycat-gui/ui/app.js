// The panel is a thin view over the daemon. It calls one Tauri command,
// `daemon`, which forwards a request to the socket and returns the reply; and
// it listens for `daemon-state`, which the Rust side polls and pushes. No
// clipboard logic lives here — the daemon decides everything.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let clips = [];          // most recent history, newest first
let paused = false;

/** Send an action to the daemon. Returns the reply, or null on failure. */
async function daemon(action, args) {
  try {
    return await invoke("daemon", { action, args: args ?? null });
  } catch (message) {
    toast(String(message));
    return null;
  }
}

function toast(text) {
  const el = $("toast");
  el.textContent = text;
  el.hidden = false;
  clearTimeout(toast._t);
  toast._t = setTimeout(() => { el.hidden = true; }, 2200);
}

function age(ms) {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

// ---- the mode strip, from the polled daemon state ------------------------

function renderState(payload) {
  const dot = $("dot");
  const conn = $("conn");
  if (!payload.connected) {
    dot.classList.add("off");
    conn.textContent = "daemon offline";
    setMode(null, null);
    return;
  }
  dot.classList.remove("off");
  conn.textContent = "connected";

  const core = payload.status?.core ?? {};
  paused = !!core.paused;
  $("pause-label").textContent = paused ? "resume" : "pause";
  setMode(core.session ?? null, core.latest ?? null);
}

function setMode(session, latest) {
  const mode = $("mode");
  const name = $("mode-name");
  const count = $("mode-count");
  const nextline = $("nextline");

  if (!session) {
    mode.classList.add("normal");
    name.textContent = paused ? "PAUSED" : "NORMAL";
    count.innerHTML = latest ? `latest · ${escape(latest.preview)}` : "";
    nextline.hidden = true;
    return;
  }

  mode.classList.remove("normal");
  name.textContent = session.mode.toUpperCase();
  if (session.state === "capturing") {
    count.innerHTML = `capturing · <b>${session.size}</b> collected`;
  } else {
    count.innerHTML = `<b>${session.cursor}</b> / ${session.size} · <b>${session.remaining}</b> left`;
  }

  const next = session.next != null ? previewFor(session.next) : null;
  if (next && session.state !== "capturing") {
    $("next-clip").textContent = next;
    nextline.hidden = false;
  } else {
    nextline.hidden = true;
  }
}

function previewFor(id) {
  const hit = clips.find((c) => c.id === id);
  return hit ? hit.preview : `#${id}`;
}

// ---- history list --------------------------------------------------------

async function refreshHistory() {
  const reply = await daemon("history.list", { limit: 8, raw: false });
  if (reply && reply.type === "clips") {
    clips = reply.clips;
    renderRows();
  }
}

function renderRows() {
  const rows = $("rows");
  rows.textContent = "";
  if (clips.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = "Nothing copied yet.";
    rows.appendChild(empty);
    return;
  }
  clips.forEach((clip, i) => {
    const row = document.createElement("button");
    row.className = "row";
    row.type = "button";
    row.innerHTML = `
      <span class="idx">${i + 1}</span>
      <span class="clip">${escape(clip.preview) || "<em>(empty)</em>"}${
        clip.duplicate_run > 1 ? `<span class="dupe">  ×${clip.duplicate_run}</span>` : ""
      }</span>
      <span class="meta">
        <span class="age">${age(clip.captured_at)}</span>
        ${clip.pinned ? `<span class="glyph pin">★</span>` : ""}
      </span>`;
    row.addEventListener("click", () => pasteClip(clip));
    rows.appendChild(row);
  });
}

async function pasteClip(clip) {
  const reply = await daemon("paste.id", { id: clip.id });
  if (reply && reply.type === "pasted") {
    toast(`pasted ${truncate(clip.preview, 32)}`);
  }
}

// ---- footer actions ------------------------------------------------------

function wireActions() {
  document.querySelectorAll(".act[data-action]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const reply = await daemon(btn.dataset.action, {});
      if (reply) {
        toast(readableAction(btn.dataset.action));
        refreshHistory();
      }
    });
  });
  $("pause").addEventListener("click", async () => {
    await daemon(paused ? "history.resume" : "history.pause", {});
  });
}

function readableAction(action) {
  return {
    "stack.start": "stack started",
    "queue.capture": "capturing a queue",
    "group.capture": "capturing a group",
  }[action] ?? "done";
}

// ---- small helpers -------------------------------------------------------

function escape(text) {
  const div = document.createElement("div");
  div.textContent = text ?? "";
  return div.innerHTML;
}
function truncate(text, n) {
  text = text ?? "";
  return text.length > n ? text.slice(0, n) + "…" : text;
}

// ---- boot ----------------------------------------------------------------

wireActions();
refreshHistory();
listen("daemon-state", (event) => renderState(event.payload));
// A light poll of history in case copies happen while the panel is open.
setInterval(refreshHistory, 1500);
