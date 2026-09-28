// The debug overlay. Polls the daemon fast: status for the live mode and next
// item, and debug.events (incrementally, via a cursor) for each binding trigger
// as it fires — the leader especially.

const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

let since = 0;
let seeded = false; // don't flash the backlog on first poll
let lastUptime = -1; // a drop means the daemon restarted
let misses = 0; // debug.events failures while status still answers
let eventsTimer = null;

async function daemon(action, args) {
  try { return await invoke("daemon", { action, args: args ?? null }); }
  catch { return null; }
}

function fmtTime(ms) {
  // seconds:centis since — but a wall clock is friendlier; show mm:ss.
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, "0");
  return `${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
function truncate(t, n) { t = t ?? ""; return t.length > n ? t.slice(0, n) + "…" : t; }
function esc(t) { const d = document.createElement("div"); d.textContent = t ?? ""; return d.innerHTML; }

async function pollStatus() {
  const body = await daemon("status");
  const dot = $("dot");
  if (!body || body.type !== "status") { dot.classList.add("off"); return; }
  dot.classList.remove("off");

  // A restarted daemon numbers its events from 1 again, so the old cursor
  // would hide everything it records — and it may support debug events where
  // the last one did not. Start over either way.
  if (body.uptime_ms < lastUptime) {
    since = 0; seeded = false; misses = 0;
    if (!eventsTimer) eventsTimer = setInterval(pollEvents, 250);
  }
  lastUptime = body.uptime_ms;
  const core = body.core ?? {};
  const s = core.session;
  if (!s) {
    $("mode").textContent = core.paused ? "PAUSED" : "NORMAL";
    $("next").textContent = core.latest ? truncate(core.latest.preview, 30) : "—";
    return;
  }
  $("mode").textContent = s.state === "capturing"
    ? `${s.mode.toUpperCase()} · capturing ${s.size}`
    : `${s.mode.toUpperCase()} · ${s.cursor}/${s.size}`;
  $("next").textContent = s.next != null ? `#${s.next}` : "—";
}

async function pollEvents() {
  const body = await daemon("debug.events", { since });
  if (!body || body.type !== "events") {
    // Status answers but this doesn't: the daemon predates debug events. Stop
    // asking — every refused request is a line in its log — until it restarts.
    if (!$("dot").classList.contains("off") && ++misses >= 8) {
      clearInterval(eventsTimer);
      eventsTimer = null;
      $("flash").className = "flash idle";
      $("flash-kind").textContent = "this daemon can't report triggers";
      $("flash-detail").textContent = "Restart it from Copycat → Settings to use the bundled one";
    }
    return;
  }
  misses = 0;
  const fresh = body.events;
  since = body.latest;
  if (!seeded) { seeded = true; renderFeed(fresh); return; } // establish cursor quietly
  if (fresh.length) {
    renderFeed(fresh, true);
    flash(fresh[fresh.length - 1]);
  }
}

const recent = [];
function renderFeed(events, append) {
  if (append) recent.push(...events); else recent.push(...events);
  while (recent.length > 7) recent.shift();
  $("feed").innerHTML = recent.slice().reverse().map((e) =>
    `<div class="e ${esc(e.kind)}"><span class="t">${fmtTime(e.at)}</span><span class="k">${esc(e.kind)}</span><span class="d">${esc(truncate(e.detail, 40))}</span></div>`
  ).join("");
}

function flash(e) {
  const box = $("flash");
  box.className = `flash ${e.kind}`;
  $("flash-kind").textContent = e.kind === "leader" || e.kind === "leader-key" ? "LEADER"
    : e.kind === "hotkey" ? "HOTKEY"
    : e.kind === "paste-chord" ? "PASTE"
    : e.kind.toUpperCase();
  $("flash-detail").textContent = e.detail;
  // restart the pulse animation
  box.classList.remove("pulse");
  void box.offsetWidth;
  box.classList.add("pulse");
}

pollStatus();
pollEvents();
setInterval(pollStatus, 400);
eventsTimer = setInterval(pollEvents, 250);
