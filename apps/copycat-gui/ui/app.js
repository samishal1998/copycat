// The panel is a thin view over the daemon. It calls one Tauri command,
// `daemon`, which forwards a request to the socket; and it listens for
// `daemon-state`, which the Rust side polls and pushes.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

const MAX_FETCH = 60;              // how deep the menu bar reaches into history
const SIZE_KEY = "menubar.pageSize";
const MIN_SIZE = 3, MAX_SIZE = 24;

let clips = [];                    // most recent, newest first
let page = 0;
let pageSize = clampSize(parseInt(localStorage.getItem(SIZE_KEY), 10) || 6);
let paused = false;

function clampSize(n) { return Math.min(MAX_SIZE, Math.max(MIN_SIZE, n || 6)); }

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

function escapeHtml(text) {
  const div = document.createElement("div");
  div.textContent = text ?? "";
  return div.innerHTML;
}
function truncate(text, n) {
  text = text ?? "";
  return text.length > n ? text.slice(0, n) + "…" : text;
}

// ---- mode strip, from the polled daemon state ----------------------------

function renderState(payload) {
  const dot = $("dot"), conn = $("dot");
  if (!payload.connected) {
    dot.classList.add("off");
    dot.title = "daemon offline";
    setMode(null, null);
    return;
  }
  dot.classList.remove("off");
  dot.title = "connected";
  const core = payload.status?.core ?? {};
  paused = !!core.paused;
  $("pause-label").textContent = paused ? "resume" : "pause";
  setMode(core.session ?? null, core.latest ?? null);
}

function setMode(session, latest) {
  const mode = $("mode"), name = $("mode-name"), count = $("mode-count"), nextline = $("nextline");
  if (!session) {
    mode.classList.add("normal");
    name.textContent = paused ? "PAUSED" : "NORMAL";
    count.innerHTML = latest ? `latest · ${escapeHtml(truncate(latest.preview, 26))}` : "";
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

// ---- paginated history ---------------------------------------------------

async function refreshHistory() {
  const reply = await daemon("history.list", { limit: MAX_FETCH, raw: false });
  if (reply && reply.type === "clips") {
    clips = reply.clips;
    const pages = Math.max(1, Math.ceil(clips.length / pageSize));
    if (page >= pages) page = pages - 1;
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
    updatePager();
    return;
  }
  const start = page * pageSize;
  clips.slice(start, start + pageSize).forEach((clip, i) => {
    const row = document.createElement("button");
    row.className = "row";
    row.type = "button";
    row.innerHTML = `
      <span class="idx">${start + i + 1}</span>
      <span class="clip">${escapeHtml(clip.preview) || "<em>(empty)</em>"}${
        clip.duplicate_run > 1 ? `<span class="dupe">  ×${clip.duplicate_run}</span>` : ""
      }</span>
      <span class="meta">
        <span class="age">${age(clip.captured_at)}</span>
        ${clip.pinned ? `<span class="glyph pin">★</span>` : ""}
      </span>`;
    row.addEventListener("click", () => pasteClip(clip));
    rows.appendChild(row);
  });
  updatePager();
}

function updatePager() {
  const total = clips.length;
  const start = total ? page * pageSize + 1 : 0;
  const end = Math.min(total, (page + 1) * pageSize);
  $("pager-text").textContent = total ? `${start}–${end} of ${total}` : "empty";
  $("prev").disabled = page === 0;
  $("next").disabled = end >= total;
  $("size-n").textContent = String(pageSize);
}

async function pasteClip(clip) {
  const reply = await daemon("paste.id", { id: clip.id });
  if (reply && reply.type === "pasted") toast(`pasted ${truncate(clip.preview, 32)}`);
}

// ---- controls ------------------------------------------------------------

function setPageSize(n) {
  pageSize = clampSize(n);
  localStorage.setItem(SIZE_KEY, String(pageSize));
  page = 0;
  renderRows();
}

function wire() {
  $("open-main").addEventListener("click", () => invoke("open_main"));
  $("prev").addEventListener("click", () => { if (page > 0) { page--; renderRows(); } });
  $("next").addEventListener("click", () => {
    if ((page + 1) * pageSize < clips.length) { page++; renderRows(); }
  });
  $("size-down").addEventListener("click", () => setPageSize(pageSize - 1));
  $("size-up").addEventListener("click", () => setPageSize(pageSize + 1));

  document.querySelectorAll(".act[data-action]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      if (await daemon(btn.dataset.action, {})) refreshHistory();
    });
  });
  $("pause").addEventListener("click", () =>
    daemon(paused ? "history.resume" : "history.pause", {}));
}

// ---- boot ----------------------------------------------------------------

wire();
refreshHistory();
listen("daemon-state", (event) => renderState(event.payload));
setInterval(refreshHistory, 1500);
