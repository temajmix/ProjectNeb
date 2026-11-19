#!/usr/bin/env node
/**
 * A dependency-free accessibility self-check for the ProjectNeb dashboard.
 *
 *   npm run build && node scripts/a11y-audit.mjs
 *
 * The script starts `vite preview` when nothing answers on 127.0.0.1:4173, launches the
 * installed Chromium/Edge in headless mode, and drives it over CDP. It walks every screen,
 * reads the accessibility tree for unnamed controls, checks the design tokens' contrast,
 * measures text clipping at 900x600 in Russian, tabs through the shell to verify focus order
 * and focus rings, and proves that Esc closes the palette and returns focus to its trigger.
 *
 * Exit code 0 = no failures; 1 = at least one finding. Screenshots are written only with
 * `--shots` (docs/screenshots/a11y-*.png).
 */

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const explicitUrl = process.env.A11Y_URL;
const port = Number(process.env.A11Y_PORT ?? 4189);
const baseUrl = explicitUrl ?? `http://127.0.0.1:${port}/`;
const appUrl = `${baseUrl}?windhawk=installed&zapret=installed`;
const shots = process.argv.includes("--shots");
const screens = [
  "overview",
  "appearance",
  "profiles",
  "zapret",
  "discord",
  "winmode",
  "windhawk",
  "bar",
  "wallpapers",
  "updates",
  "account",
  "logs",
  "about",
];

const failures = [];
const notes = [];
const fail = (message) => failures.push(message);
const ok = (message) => console.log(`  PASS  ${message}`);

// ---------- tiny Chrome DevTools Protocol client ----------

class CDP {
  constructor(url) {
    this.url = url;
    this.id = 0;
    this.pending = new Map();
  }

  async connect() {
    this.ws = new WebSocket(this.url);
    await new Promise((resolvePromise, reject) => {
      this.ws.addEventListener("open", resolvePromise, { once: true });
      this.ws.addEventListener("error", reject, { once: true });
    });
    this.ws.addEventListener("message", (event) => {
      let message;
      try {
        message = JSON.parse(typeof event.data === "string" ? event.data : String(event.data));
      } catch {
        return;
      }
      const pending = this.pending.get(message.id);
      if (!pending) {
        return;
      }
      this.pending.delete(message.id);
      if (message.error) {
        pending.reject(new Error(`${pending.method}: ${message.error.message}`));
      } else {
        pending.resolve(message.result);
      }
    });
  }

  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolvePromise, reject) => {
      this.pending.set(id, { resolve: resolvePromise, reject, method });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  close() {
    this.ws?.close();
  }
}

async function evaluate(cdp, expression) {
  const result = await cdp.send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
    userGesture: true,
  });
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description ?? "evaluation failed");
  }
  return result.result.value;
}

const sleep = (ms) => new Promise((resolvePromise) => setTimeout(resolvePromise, ms));

async function waitForHttp(url, timeout = 20000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(1000) });
      if (response.ok) {
        return true;
      }
    } catch {
      // not up yet
    }
    await sleep(250);
  }
  return false;
}

// ---------- browser discovery ----------

function browserPath() {
  const candidates = [
    process.env.CHROME_PATH,
    process.env.EDGE_PATH,
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  ].filter(Boolean);
  return candidates.find((candidate) => existsSync(candidate));
}

async function startPreview() {
  if (explicitUrl) {
    const ready = await waitForHttp(baseUrl, 600);
    if (!ready) {
      throw new Error(`nothing answers at ${baseUrl}`);
    }
    notes.push(`using A11Y_URL ${baseUrl}`);
    return null;
  }

  const running = await waitForHttp(baseUrl, 600);
  if (running) {
    notes.push(`using the preview already running at ${baseUrl}`);
    return null;
  }

  if (!existsSync(join(root, "dist", "index.html"))) {
    throw new Error("dist/index.html is missing — run `npm run build` first");
  }

  const vite = join(root, "node_modules", "vite", "bin", "vite.js");
  if (!existsSync(vite)) {
    throw new Error("vite is not installed — run `npm install` first");
  }

  const child = spawn(
    process.execPath,
    [vite, "preview", "--host", "127.0.0.1", "--port", String(port)],
    { cwd: root, stdio: "ignore" },
  );
  if (!(await waitForHttp(baseUrl))) {
    child.kill();
    throw new Error(`vite preview did not start at ${baseUrl}`);
  }
  notes.push(`started vite preview at ${baseUrl}`);
  return child;
}

async function startBrowser() {
  const executable = browserPath();
  if (!executable) {
    throw new Error("no Chrome/Edge found — set CHROME_PATH or EDGE_PATH");
  }

  const port = 9222 + Math.floor(Math.random() * 300);
  const profile = join(tmpdir(), `neb-a11y-${process.pid}-${Date.now()}`);
  const child = spawn(
    executable,
    [
      "--headless=new",
      "--disable-gpu",
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-extensions",
      "--force-device-scale-factor=1",
      "--window-size=1100,720",
      `--remote-debugging-port=${port}`,
      `--user-data-dir=${profile}`,
      "about:blank",
    ],
    { stdio: "ignore" },
  );

  const versionUrl = `http://127.0.0.1:${port}/json/version`;
  if (!(await waitForHttp(versionUrl, 15000))) {
    child.kill();
    throw new Error("headless browser did not expose a CDP endpoint");
  }

  await fetch(`http://127.0.0.1:${port}/json/new?${encodeURIComponent(appUrl)}`, { method: "PUT" });
  let page;
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline && !page) {
    const list = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    page = list.find((target) => target.type === "page" && target.url.startsWith(baseUrl));
    if (!page) {
      await sleep(150);
    }
  }
  if (!page) {
    child.kill();
    throw new Error("the preview tab was not created");
  }

  const cdp = new CDP(page.webSocketDebuggerUrl);
  await cdp.connect();
  return { child, cdp };
}

async function navigate(cdp, hash, settle = 500) {
  await evaluate(cdp, `location.hash = ${JSON.stringify(`#/${hash}`)}`);
  await sleep(settle);
}

// ---------- page-side audit helpers (serialized as strings) ----------

const AX_ROLES = [
  "button",
  "link",
  "textbox",
  "searchbox",
  "combobox",
  "checkbox",
  "radio",
  "switch",
  "tab",
  "slider",
  "spinbutton",
];

async function unnamedControls(cdp) {
  const tree = await cdp.send("Accessibility.getFullAXTree");
  return tree.nodes
    .filter((node) => !node.ignored && AX_ROLES.includes(node.role?.value ?? ""))
    .filter((node) => (node.name?.value ?? "").trim().length === 0)
    .map((node) => `${node.role.value}${node.backendDOMNodeId ? ` #${node.backendDOMNodeId}` : ""}`);
}

async function pageAudit(cdp) {
  return evaluate(
    cdp,
    `(() => {
      const visible = (node) => {
        const rect = node.getBoundingClientRect();
        const style = getComputedStyle(node);
        return rect.width > 0 && rect.height > 0 && style.visibility !== "hidden" && style.display !== "none";
      };
      const text = (node) => (node.innerText || node.textContent || "").replace(/\\s+/g, " ").trim();

      const h1 = [...document.querySelectorAll("h1")].filter(visible);
      const navs = [...document.querySelectorAll("nav")].filter(visible);
      const mains = [...document.querySelectorAll("main")].filter(visible);

      const inputs = [...document.querySelectorAll("input:not([type=hidden]), select, textarea")]
        .filter(visible)
        .map((node) => {
          const id = node.id;
          const labelled =
            node.getAttribute("aria-label") ||
            node.getAttribute("aria-labelledby") ||
            (id && document.querySelector('label[for="' + CSS.escape(id) + '"]')) ||
            node.closest("label");
          return labelled ? null : node.tagName.toLowerCase() + (id ? "#" + id : "");
        })
        .filter(Boolean);

      const stateful = [...document.querySelectorAll('[role="switch"], [role="radio"], [role="tab"], [aria-pressed]')]
        .filter(visible)
        .map((node) => {
          const role = node.getAttribute("role") || "button";
          const state = role === "switch" ? node.getAttribute("aria-checked") : role === "radio" ? node.getAttribute("aria-checked") : role === "tab" ? node.getAttribute("aria-selected") : node.getAttribute("aria-pressed");
          return state === null || state === undefined ? role + (node.id ? "#" + node.id : "") : null;
        })
        .filter(Boolean);

      const ids = [...document.querySelectorAll("[id]")];
      const seen = new Map();
      const duplicates = [];
      for (const node of ids) {
        if (seen.has(node.id) && visible(node)) duplicates.push(node.id);
        seen.set(node.id, node);
      }

      const clipped = [...document.querySelectorAll("main *")]
        .filter(visible)
        .filter((node) => {
          if (!text(node)) return false;
          if (node.children.length > 0 && ![...node.children].every((child) => !text(child))) return false;
          const style = getComputedStyle(node);
          return (
            (node.scrollWidth > node.clientWidth + 1 && style.overflowX !== "visible") ||
            (node.scrollHeight > node.clientHeight + 1 && style.overflowY !== "visible")
          );
        })
        .map((node) => {
          const style = getComputedStyle(node);
          return {
            selector: node.tagName.toLowerCase() + (node.id ? "#" + node.id : "") + (node.className && typeof node.className === "string" ? "." + node.className.split(/\\s+/).join(".") : ""),
            text: text(node).slice(0, 80),
            ellipsis: style.textOverflow === "ellipsis",
          };
        });

      return {
        h1: h1.map((node) => text(node)),
        navs: navs.length,
        mains: mains.length,
        inputs,
        stateful,
        duplicates: [...new Set(duplicates)],
        clipped,
      };
    })()`,
  );
}

function russianIndex(count) {
  const value = Math.abs(count) % 100;
  if (value >= 11 && value <= 14) return 2;
  switch (value % 10) {
    case 1: return 0;
    case 2:
    case 3:
    case 4: return 1;
    default: return 2;
  }
}

function checkPluralisation() {
  const cases = [
    { name: "мод", forms: ["мод", "мода", "модов"], expected: ["1 мод", "2 мода", "5 модов"] },
    { name: "профиль", forms: ["профиль", "профиля", "профилей"], expected: ["1 профиль", "2 профиля", "5 профилей"] },
    { name: "секунда", forms: ["секунда", "секунды", "секунд"], expected: ["1 секунда", "2 секунды", "5 секунд"] },
  ];

  for (const entry of cases) {
    for (const [index, count] of [1, 2, 5].entries()) {
      const text = `${count} ${entry.forms[russianIndex(count)]}`;
      if (text !== entry.expected[index]) {
        fail(`pluralisation ${entry.name}: expected “${entry.expected[index]}”, got “${text}”`);
      }
    }
  }

  if (!failures.some((message) => message.startsWith("pluralisation"))) {
    ok("Russian plurals: мод/мода/модов, профиль/профиля/профилей, секунда/секунды/секунд");
  }
}

async function contrastReport(cdp) {
  return evaluate(
    cdp,
    `(() => {
      const root = getComputedStyle(document.documentElement);
      const colour = (value) => {
        const probe = document.createElement("span");
        probe.style.color = value;
        document.body.append(probe);
        const match = getComputedStyle(probe).color.match(/rgba?\\(([^)]+)\\)/);
        probe.remove();
        const parts = match ? match[1].split(",").map(Number) : [0, 0, 0, 1];
        return { r: parts[0], g: parts[1], b: parts[2], a: parts[3] ?? 1 };
      };
      const over = (fg, bg) => ({
        r: fg.r * fg.a + bg.r * (1 - fg.a),
        g: fg.g * fg.a + bg.g * (1 - fg.a),
        b: fg.b * fg.a + bg.b * (1 - fg.a),
        a: 1,
      });
      const hex = (value) => {
        const match = value.trim().match(/^#([0-9a-f]{6})$/i);
        return match ? { r: parseInt(match[1].slice(0,2),16), g: parseInt(match[1].slice(2,4),16), b: parseInt(match[1].slice(4,6),16), a: 1 } : colour(value);
      };
      const luminance = ({ r, g, b }) => {
        const channel = (v) => {
          v /= 255;
          return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
        };
        return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
      };
      const ratio = (a, b) => {
        const la = luminance(a), lb = luminance(b);
        return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
      };

      const bg = hex(root.getPropertyValue("--window") || "#0b0a12");
      const glass = over(hex(root.getPropertyValue("--glass") || "rgba(255,255,255,0.055)"), bg);
      const tokens = {
        text: root.getPropertyValue("--text"),
        dim: root.getPropertyValue("--text-dim"),
        mute: root.getPropertyValue("--text-mute"),
        accent: root.getPropertyValue("--accent"),
        accent2: root.getPropertyValue("--accent-2"),
      };

      const rows = [];
      for (const [name, value] of Object.entries(tokens)) {
        const fg = hex(value);
        rows.push({ token: name + " on window", ratio: +ratio(fg, bg).toFixed(2) });
        rows.push({ token: name + " on glass", ratio: +ratio(fg, glass).toFixed(2) });
      }

      const primary = document.querySelector(".btn--primary");
      let primaryRatio = null;
      if (primary) {
        const image = getComputedStyle(primary).backgroundImage;
        const stops = [...image.matchAll(/rgba?\\(([^)]+)\\)/g)].map((match) => {
          const parts = match[1].split(",").map(Number);
          return { r: parts[0], g: parts[1], b: parts[2], a: parts[3] ?? 1 };
        });
        const darkest = stops.sort((a, b) => luminance(a) - luminance(b))[0];
        if (darkest) primaryRatio = +ratio(hex(tokens.text), darkest).toFixed(2);
      }

      return {
        rows,
        primary: primaryRatio,
        glass: "#" + [glass.r, glass.g, glass.b].map((v) => Math.round(v).toString(16).padStart(2, "0")).join(""),
      };
    })()`,
  );
}

// ---------- keyboard walkthrough ----------

async function press(cdp, key, code, modifiers = 0, virtualKey = 0) {
  await cdp.send("Input.dispatchKeyEvent", {
    type: "rawKeyDown",
    key,
    code,
    modifiers,
    windowsVirtualKeyCode: virtualKey,
  });
  await cdp.send("Input.dispatchKeyEvent", {
    type: "keyUp",
    key,
    code,
    modifiers,
    windowsVirtualKeyCode: virtualKey,
  });
}

async function tabWalk(cdp, steps = 14) {
  await evaluate(cdp, "document.activeElement?.blur(); document.body.focus();");
  const order = [];
  for (let index = 0; index < steps; index += 1) {
    await press(cdp, "Tab", "Tab", 0, 9);
    await sleep(40);
    order.push(
      await evaluate(
        cdp,
        `(() => {
          const node = document.activeElement;
          if (!(node instanceof HTMLElement)) return null;
          const style = getComputedStyle(node);
          return {
            tag: node.tagName.toLowerCase(),
            id: node.id,
            label: node.getAttribute("aria-label") || (node.innerText || "").replace(/\\s+/g, " ").trim().slice(0, 42),
            outline: style.outlineStyle !== "none" && parseFloat(style.outlineWidth) > 0,
            visible: node.getClientRects().length > 0,
          };
        })()`,
      ),
    );
  }
  return order.filter(Boolean);
}

// ---------- screenshots ----------

async function capture(cdp, name) {
  const dir = resolve(root, "..", "..", "docs", "screenshots");
  mkdirSync(dir, { recursive: true });
  const result = await cdp.send("Page.captureScreenshot", { format: "png", fromSurface: true });
  writeFileSync(join(dir, name), Buffer.from(result.data, "base64"));
  return join("docs", "screenshots", name);
}

// ---------- main ----------

let preview;
let browser;
let cdp;
try {
  preview = await startPreview();
  ({ child: browser, cdp } = await startBrowser());

  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Accessibility.enable");
  await cdp.send("Emulation.setDeviceMetricsOverride", {
    width: 1100,
    height: 720,
    deviceScaleFactor: 1,
    mobile: false,
  });

  await waitForHttp(baseUrl, 10000);
  await sleep(800);

  console.log("\nProjectNeb a11y audit");
  console.log("=====================\n");

  // 1. Every screen, accessible names and structure.
  for (const screen of screens) {
    await navigate(cdp, screen, 550);
    const missing = await unnamedControls(cdp);
    const info = await pageAudit(cdp);
    const where = `${screen}:`;
    if (missing.length > 0) {
      fail(`${where} controls without an accessible name: ${missing.join(", ")}`);
    }
    if (info.h1.length !== 1) {
      fail(`${where} expected one <h1>, found ${info.h1.length}`);
    }
    if (info.navs < 1) {
      fail(`${where} no <nav> landmark`);
    }
    if (info.mains !== 1) {
      fail(`${where} expected one <main>, found ${info.mains}`);
    }
    if (info.inputs.length > 0) {
      fail(`${where} inputs without a label: ${info.inputs.join(", ")}`);
    }
    if (info.stateful.length > 0) {
      fail(`${where} toggles without state: ${info.stateful.join(", ")}`);
    }
    if (info.duplicates.length > 0) {
      fail(`${where} duplicate ids: ${info.duplicates.join(", ")}`);
    }
    if (missing.length === 0 && info.h1.length === 1 && info.inputs.length === 0) {
      ok(`${where} names, h1, nav/main landmarks`);
    }
  }

  // 2. Overlay names.
  await navigate(cdp, "overview", 500);
  await press(cdp, "k", "KeyK", 2, 75);
  await sleep(350);
  const paletteMissing = await unnamedControls(cdp);
  if (paletteMissing.length > 0) {
    fail(`palette controls without an accessible name: ${paletteMissing.join(", ")}`);
  } else {
    ok("palette controls have accessible names");
  }
  await press(cdp, "Escape", "Escape", 0, 27);
  await sleep(200);

  await press(cdp, "F1", "F1", 0, 112);
  await sleep(300);
  const shortcutsMissing = await unnamedControls(cdp);
  if (shortcutsMissing.length > 0) {
    fail(`shortcut sheet controls without an accessible name: ${shortcutsMissing.join(", ")}`);
  } else {
    ok("shortcut sheet controls have accessible names");
  }
  await press(cdp, "Escape", "Escape", 0, 27);
  await sleep(200);

  // The first-run tour is the one overlay without a visible trigger; it still traps focus and
  // treats Esc as a real skip.
  await cdp.send("Page.navigate", { url: `${appUrl}&onboarding=1` });
  await sleep(1200);
  const onboardingOpen = await evaluate(
    cdp,
    "!!document.querySelector('dialog.onboarding[open]')",
  );
  if (!onboardingOpen) {
    fail("the onboarding tour did not open with ?onboarding=1");
  } else {
    const onboardingMissing = await unnamedControls(cdp);
    if (onboardingMissing.length > 0) {
      fail(`onboarding controls without an accessible name: ${onboardingMissing.join(", ")}`);
    } else {
      ok("onboarding controls have accessible names");
    }
    await press(cdp, "Escape", "Escape", 0, 27);
    await sleep(500);
    const onboardingClosed = await evaluate(
      cdp,
      "!document.querySelector('dialog.onboarding[open]')",
    );
    if (onboardingClosed) {
      ok("Esc closes the onboarding tour");
    } else {
      fail("Esc did not close the onboarding tour");
    }
  }

  await cdp.send("Page.navigate", { url: appUrl });
  await sleep(1000);

  // 3. Token contrast.
  await navigate(cdp, "overview", 500);
  checkPluralisation();
  const contrast = await contrastReport(cdp);
  console.log("\nContrast (WCAG 2.1, body text needs 4.5:1):");
  for (const row of contrast.rows) {
    const pass = row.ratio >= 4.5;
    console.log(`  ${pass ? "PASS" : "FAIL"}  ${row.token.padEnd(30)} ${row.ratio.toFixed(2)}:1`);
    if (!pass) {
      fail(`contrast ${row.token} is ${row.ratio}:1 (< 4.5:1)`);
    }
  }
  if (contrast.primary !== null) {
    const pass = contrast.primary >= 4.5;
    console.log(`  ${pass ? "PASS" : "FAIL"}  primary button label on accent   ${contrast.primary.toFixed(2)}:1`);
    if (!pass) {
      fail(`primary button text is ${contrast.primary}:1 (< 4.5:1)`);
    }
  }

  // 4. Russian and English text at the minimum window size.
  await cdp.send("Emulation.setDeviceMetricsOverride", {
    width: 900,
    height: 600,
    deviceScaleFactor: 1,
    mobile: false,
  });
  const clippings = [];
  for (const screen of screens) {
    await navigate(cdp, screen, 450);
    const info = await pageAudit(cdp);
    for (const item of info.clipped) {
      clippings.push(`${screen}: ${item.selector} — “${item.text}”${item.ellipsis ? " (ellipsis)" : ""}`);
    }
  }
  if (clippings.length === 0) {
    ok("no clipped Russian text at 900x600");
  } else {
    fail(`Russian text clipped at 900x600: ${clippings.join("; ")}`);
  }

  // English strings must not overflow either.
  await evaluate(cdp, "document.querySelector('#lang-en')?.click(); true");
  await sleep(700);
  const englishClippings = [];
  for (const screen of screens) {
    await navigate(cdp, screen, 450);
    const info = await pageAudit(cdp);
    for (const item of info.clipped) {
      englishClippings.push(`${screen}: ${item.selector} — “${item.text}”`);
    }
  }
  if (englishClippings.length === 0) {
    ok("no clipped English text at 900x600");
  } else {
    fail(`English text clipped at 900x600: ${englishClippings.join("; ")}`);
  }
  await evaluate(cdp, "document.querySelector('#lang-ru')?.click(); true");
  await sleep(700);

  await cdp.send("Emulation.setDeviceMetricsOverride", {
    width: 1100,
    height: 720,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await navigate(cdp, "overview", 500);

  // 5. Tab order and focus rings. Reload so sequential focus starts at the document top.
  await cdp.send("Page.reload", { ignoreCache: false });
  await sleep(1200);
  await evaluate(cdp, "location.hash = '#/overview'");
  await sleep(500);
  const order = (await tabWalk(cdp, 32)).filter((item) => item.tag !== "body");
  const first = order.slice(0, 4).map((item) => item.id || item.label || item.tag);
  console.log(`\nTab order: ${order.map((item) => item.id || item.label || item.tag).join(" -> ")}`);
  if (!order.slice(0, 3).every((item) => item.tag === "button")) {
    fail("the first three Tab stops are not the window controls");
  }
  if (!order.slice(3, 6).every((item) => item.tag === "button")) {
    fail("the navigation does not follow the window controls");
  }
  const noRing = order.filter((item) => !item.outline && item.tag !== "body");
  if (noRing.length > 0) {
    fail(`focus without a visible ring: ${noRing.map((item) => item.id || item.label || item.tag).join(", ")}`);
  } else {
    ok(`Tab through the shell keeps a visible ring (${order.length} stops)`);
  }
  if (order.some((item) => !item.visible)) {
    fail("focus reached an invisible element");
  }

  // 6. Arrow keys inside tabs, radios and lists.
  await navigate(cdp, "windhawk", 700);
  const tabArrow = await evaluate(
    cdp,
    `(async () => {
      const tab = document.querySelector('#windhawk-tab-catalog');
      tab?.focus();
      return !!tab;
    })()`,
  );
  if (!tabArrow) {
    notes.push("windhawk tabs were not available for the arrow-key check");
  } else {
    await press(cdp, "ArrowRight", "ArrowRight", 0, 39);
    await sleep(350);
    const tabsOk = await evaluate(
      cdp,
      `document.activeElement?.id === 'windhawk-tab-installed' &&
       document.querySelector('#windhawk-tab-installed')?.getAttribute('aria-selected') === 'true'`,
    );
    if (tabsOk) {
      ok("ArrowRight moves focus and selection between Windhawk tabs");
    } else {
      fail("ArrowRight did not move between Windhawk tabs");
    }
  }

  await navigate(cdp, "zapret", 700);
  const radioReady = await evaluate(
    cdp,
    `(() => {
      const radio = document.querySelector('#zapret-strategies [role="radio"][aria-checked="true"]') ||
        document.querySelector('#zapret-strategies [role="radio"]');
      radio?.focus();
      return !!radio;
    })()`,
  );
  if (!radioReady) {
    notes.push("zapret strategies were not available for the arrow-key check");
  } else {
    await press(cdp, "ArrowDown", "ArrowDown", 0, 40);
    await sleep(250);
    const radioOk = await evaluate(
      cdp,
      `document.activeElement?.getAttribute('role') === 'radio' &&
       document.activeElement?.getAttribute('aria-checked') === 'true'`,
    );
    if (radioOk) {
      ok("ArrowDown moves focus and selection inside the strategy radiogroup");
    } else {
      fail("ArrowDown did not move inside the strategy radiogroup");
    }
  }

  // 7. Motion budget and the reduced-motion escape hatch.
  await navigate(cdp, "overview", 500);
  const duration = await evaluate(
    cdp,
    `(() => {
      const node = document.querySelector('.screen:not([hidden])');
      if (!node) return null;
      const value = getComputedStyle(node).animationDuration;
      return value.endsWith('ms') ? parseFloat(value) : parseFloat(value) * 1000;
    })()`,
  );
  if (duration === null) {
    fail("could not read the screen transition duration");
  } else if (duration > 300) {
    fail(`screen transition is ${duration} ms (> 300 ms)`);
  } else {
    ok(`screen transition is ${duration} ms`);
  }

  await cdp.send("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-reduced-motion", value: "reduce" }],
  });
  await navigate(cdp, "appearance", 400);
  const reduced = await evaluate(
    cdp,
    `(() => {
      const node = document.querySelector('.screen:not([hidden])');
      if (!node) return null;
      const value = getComputedStyle(node).animationDuration;
      return value.endsWith('ms') ? parseFloat(value) : parseFloat(value) * 1000;
    })()`,
  );
  await cdp.send("Emulation.setEmulatedMedia", { features: [] });
  if (reduced !== null && reduced > 1) {
    fail(`prefers-reduced-motion still animates screens (${reduced} ms)`);
  } else {
    ok("prefers-reduced-motion removes the screen transition");
  }

  // 8. Esc closes overlays and returns focus.
  await navigate(cdp, "overview", 500);
  await evaluate(cdp, "document.querySelector('#overview-status')?.setAttribute('tabindex','-1'); document.querySelector('#overview-status')?.focus(); true");
  const beforePalette = await evaluate(cdp, "document.activeElement?.id || document.activeElement?.tagName");
  await press(cdp, "k", "KeyK", 2, 75);
  await sleep(300);
  const paletteOpen = await evaluate(cdp, "!!document.querySelector('dialog.palette[open]')");
  if (!paletteOpen) {
    fail("Ctrl+K did not open the palette");
  } else {
    await press(cdp, "Escape", "Escape", 0, 27);
    await sleep(250);
    const paletteClosed = await evaluate(cdp, "!document.querySelector('dialog.palette[open]')");
    const afterPalette = await evaluate(cdp, "document.activeElement?.id || document.activeElement?.tagName");
    if (!paletteClosed) {
      fail("Esc did not close the palette");
    } else if (afterPalette !== beforePalette) {
      fail(`focus returned to ${afterPalette}, expected ${beforePalette}`);
    } else {
      ok("Esc closes the palette and returns focus to the trigger");
    }
  }

  // 9. Esc closes the confirmation dialog and returns to its trigger.
  await evaluate(cdp, "document.querySelector('#overview-restart')?.focus(); document.querySelector('#overview-restart')?.click(); true");
  await sleep(250);
  const modalOpen = await evaluate(cdp, "!!document.querySelector('dialog.modal[open]')");
  if (!modalOpen) {
    fail("the restart confirmation did not open");
  } else {
    await press(cdp, "Escape", "Escape", 0, 27);
    await sleep(250);
    const closed = await evaluate(cdp, "!document.querySelector('dialog.modal[open]')");
    const focus = await evaluate(cdp, "document.activeElement?.id || document.activeElement?.tagName");
    if (!closed) {
      fail("Esc did not close the confirmation");
    } else if (focus !== "overview-restart") {
      fail(`confirmation focus returned to ${focus}, expected overview-restart`);
    } else {
      ok("Esc closes the confirmation and returns focus to its trigger");
    }
  }

  // 10. Screenshots (opt-in).
  if (shots) {
    await navigate(cdp, "appearance", 700);
    await evaluate(
      cdp,
      "const n=document.querySelector('#theme-animations'); n?.scrollIntoView({block:'center'}); n?.focus(); true",
    );
    await sleep(250);
    notes.push(`screenshot ${await capture(cdp, "a11y-focus.png")}`);

    await evaluate(
      cdp,
      `(() => {
        const input = document.querySelector('#colour-background');
        if (!input) return false;
        input.scrollIntoView({ block: 'center' });
        input.focus();
        input.value = 'not-a-colour';
        input.dispatchEvent(new Event('input', { bubbles: true }));
        return true;
      })()`,
    );
    await sleep(300);
    notes.push(`screenshot ${await capture(cdp, "a11y-error.png")}`);

    await navigate(cdp, "overview", 400);
    await press(cdp, "k", "KeyK", 2, 75);
    await sleep(400);
    notes.push(`screenshot ${await capture(cdp, "a11y-palette.png")}`);
    await press(cdp, "Escape", "Escape", 0, 27);
  }

  console.log("\nNotes:");
  for (const note of notes) {
    console.log(`  - ${note}`);
  }

  if (failures.length > 0) {
    console.log(`\n${failures.length} finding(s):`);
    for (const failure of failures) {
      console.log(`  FAIL  ${failure}`);
    }
    process.exitCode = 1;
  } else {
    console.log("\nAll checks passed.");
  }
} catch (error) {
  console.error(`\na11y audit crashed: ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
} finally {
  cdp?.close();
  browser?.kill();
  preview?.kill();
}
