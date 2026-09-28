// A session's items as a reorderable list: drag a row, or use its ▲/▼ buttons
// (the keyboard path, and the fallback wherever drag-and-drop misbehaves).
// Items already pasted stay put, dimmed; nothing can move in front of them.
//
// Shared by the main window's Session screen and the floating panel. Each
// passes its own `daemon` bridge, so a refused move surfaces the way that
// window reports errors.

window.SessionList = function (container, daemon) {
  let session = null; // the latest summary from the status poll
  let items = [];
  let key = ""; // what is drawn, so a poll that changed nothing redraws nothing
  let dragFrom = null;
  let busy = false;

  const esc = (t) => { const d = document.createElement("div"); d.textContent = t ?? ""; return d.innerHTML; };
  const attr = (t) => esc(t).replace(/"/g, "&quot;");

  // Called with each status tick's session. Refetches the items but redraws
  // only when they changed, and never mid-drag (a redraw would end the drag).
  async function update(s) {
    session = s;
    if (!s) { key = ""; items = []; container.innerHTML = ""; return; }
    if (busy || dragFrom != null) return;
    busy = true;
    const reply = await daemon("session.items");
    busy = false;
    if (!reply || reply.type !== "clips" || dragFrom != null || !session) return;
    const k = `${session.cursor}|${reply.clips.map((c) => c.id).join(",")}`;
    if (k === key) return;
    key = k;
    items = reply.clips;
    render();
  }

  function render() {
    const cursor = session.cursor;
    const last = items.length - 1;
    if (!items.length) {
      container.innerHTML = `<li class="sl-empty">Nothing collected yet — copy something.</li>`;
      return;
    }
    container.innerHTML = items.map((c, i) => {
      const done = i < cursor;
      const tag = done ? `<span class="sl-tag">pasted</span>`
        : i === cursor ? `<span class="sl-tag next">next</span>` : `<span class="sl-tag"></span>`;
      const tools = done ? `<span></span>` : `<span class="sl-tools">
          <button class="sl-btn" data-d="-1" aria-label="Move item ${i + 1} up"${i === cursor ? " disabled" : ""}>▲</button>
          <button class="sl-btn" data-d="1" aria-label="Move item ${i + 1} down"${i === last ? " disabled" : ""}>▼</button>
        </span>`;
      return `<li class="sl-row${done ? " done" : ""}${i === cursor ? " next" : ""}" data-i="${i}"${done ? "" : ' draggable="true"'}>
        <span class="sl-grip" aria-hidden="true">${done ? "" : "⠿"}</span>
        <span class="sl-n">${i + 1}</span>
        <span class="sl-text" title="${attr(c.preview)}">${esc(c.preview) || "<em>(empty)</em>"}</span>
        ${tag}${tools}
      </li>`;
    }).join("");
    wire();
  }

  function wire() {
    container.querySelectorAll(".sl-btn").forEach((b) => {
      b.onclick = () => {
        const i = Number(b.closest(".sl-row").dataset.i);
        const d = Number(b.dataset.d);
        move(i, i + d, d);
      };
    });

    container.querySelectorAll('.sl-row[draggable="true"]').forEach((row) => {
      const after = (e) => e.clientY > row.getBoundingClientRect().top + row.offsetHeight / 2;
      row.addEventListener("dragstart", (e) => {
        dragFrom = Number(row.dataset.i);
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", String(dragFrom));
        row.classList.add("dragging");
      });
      row.addEventListener("dragend", () => {
        dragFrom = null;
        row.classList.remove("dragging");
        clearMarks();
      });
      row.addEventListener("dragover", (e) => {
        if (dragFrom == null) return;
        e.preventDefault();
        clearMarks();
        row.classList.add(after(e) ? "drop-after" : "drop-before");
      });
      row.addEventListener("drop", (e) => {
        e.preventDefault();
        if (dragFrom == null) return;
        let to = Number(row.dataset.i) + (after(e) ? 1 : 0);
        const from = dragFrom;
        if (to > from) to -= 1; // the dragged row leaves its old slot first
        dragFrom = null;
        clearMarks();
        move(from, to);
      });
    });
  }

  function clearMarks() {
    container.querySelectorAll(".drop-before, .drop-after")
      .forEach((r) => r.classList.remove("drop-before", "drop-after"));
  }

  // `d` is set when a ▲/▼ button moved the item, so keyboard focus can follow
  // it instead of falling to the page when the list redraws.
  async function move(from, to, d) {
    to = Math.max(session.cursor, Math.min(items.length - 1, to));
    if (from === to || !items[from]) return;
    await daemon("session.move", { from, to, id: items[from].id });
    key = ""; // redraw from the daemon's order, whether the move landed or was refused
    await update(session);
    if (d) {
      const row = container.querySelector(`.sl-row[data-i="${to}"]`);
      const btn = row && (row.querySelector(`.sl-btn[data-d="${d}"]:not(:disabled)`)
        || row.querySelector(".sl-btn:not(:disabled)"));
      if (btn) btn.focus();
    }
  }

  return { update };
};
