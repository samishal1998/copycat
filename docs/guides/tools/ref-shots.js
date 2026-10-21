// Reference screenshots of the Tauri UI with a fake daemon.
// usage: serve apps/<gui>/ui on 127.0.0.1:8765, then: node ref-shots.js <path to playwright package>
// (mock-tauri.js must sit beside it; it fakes window.__TAURI__ with sample data)
const fs = require("fs"), path = require("path");
const { chromium } = require(process.argv[2]);
const D = __dirname, mock = fs.readFileSync(path.join(D, "mock-tauri.js"), "utf8");
(async () => {
  const b = await chromium.launch(), errors = [];
  const shot = async (url, vw, vh, out, pre = "", click = null) => {
    const p = await b.newPage({ viewport: { width: vw, height: vh }, deviceScaleFactor: 2 });
    await p.addInitScript({ content: pre + mock }); p.on("pageerror", (e) => errors.push(url + ": " + e));
    await p.goto("http://127.0.0.1:8765/" + url); await p.waitForTimeout(900);
    if (click) { await p.click(click); await p.waitForTimeout(700); }
    await p.screenshot({ path: path.join(D, out) }); await p.close();
  };
  await shot("main.html", 940, 640, "main-history.png");
  await shot("main.html", 940, 640, "main-session.png", "", '.nav[data-screen="session"]');
  await shot("main.html", 940, 640, "main-bindings.png", "", '.nav[data-screen="bindings"]');
  await shot("main.html", 940, 640, "main-settings.png", "window.__perm = true;", '.nav[data-screen="settings"]');
  await shot("index.html", 392, 588, "dropdown.png");
  await shot("floating.html", 340, 500, "floating-panel.png", "", "#bubble");
  console.log(JSON.stringify({ errors })); await b.close();
})();
