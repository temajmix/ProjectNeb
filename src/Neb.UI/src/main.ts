import "./styles/tokens.css";
import "./styles/fonts.css";
import "./styles/app.css";
import "./styles/sections.css";

import { createHost } from "./host";
import type { CallResult } from "./host";
import type { AppContext, Screen } from "./context";
import { createI18n, browserLanguage, type Language } from "./i18n";
import {
  configSectionParams,
  configSetParams,
  defaultConfig,
  normalizeConfig,
  normalizeShellState,
  normalizeWindowsState,
  readMetrics,
  screenOrder,
  screenShortcut,
  setConfigPath,
  windowsToggleMap,
  type AppConfig,
  type Metrics,
  type ScreenId,
  type ShellState,
  type WindowsState,
} from "./state";
import { readUpdateCheck } from "./components/updateResult";
import { createTitleBar } from "./components/TitleBar";
import { createSidebar, resolveScreen, screenMeta } from "./components/Sidebar";
import { showToast } from "./components/toast";
import { createPalette, type PaletteEntry, type PaletteTarget } from "./components/Palette";
import { createOnboarding, type OnboardingSelection } from "./components/Onboarding";
import { createShortcuts } from "./components/Shortcuts";
import { CAPTION_BAND_HEIGHT, CAPTION_BAND_PROPERTY } from "./theme/contract";
import { createOverview } from "./screens/Overview";
import { createAppearanceScreen } from "./screens/Appearance";
import { createProfilesScreen } from "./screens/Profiles";
import { createBarScreen } from "./screens/BarWidgets";
import { createZapretScreen } from "./screens/Zapret";
import { createDiscordScreen } from "./screens/Discord";
import { createWindowsModeScreen } from "./screens/WindowsMode";
import { createWindhawkScreen } from "./screens/Windhawk";
import { createUpdatesScreen } from "./screens/Updates";
import { createAccountScreen } from "./screens/Account";
import { createLogsScreen } from "./screens/Logs";
import { createAboutScreen } from "./screens/About";
import { createWallpapersSection } from "./components/Wallpapers";

const i18n = createI18n(browserLanguage());
document.documentElement.lang = i18n.language;
document.documentElement.style.setProperty(CAPTION_BAND_PROPERTY, `${CAPTION_BAND_HEIGHT}px`);

const root = document.getElementById("app");
if (!root) {
  throw new Error("#app is missing from index.html");
}

const host = createHost();

let config: AppConfig = defaultConfig();
let shell: ShellState | null = null;
let windowsState: WindowsState | null = null;
let metrics: Metrics | null = host.previewMetrics();
let version = __APP_VERSION__;
let channel = "beta";
let configLoaded = false;

function adoptConfig(raw: unknown): void {
  config = normalizeConfig(raw);
  configLoaded = true;
  applyMotion();
}

function applyMotion(): void {
  document.documentElement.classList.toggle("no-motion", !config.animations.enabled);
}

async function saveConfig(path: string, value: unknown): Promise<boolean> {
  const result = await host.call<unknown>("config.set", configSetParams(path, value));
  if (!result.ok) {
    notifyFailure(result);
    return false;
  }

  adoptSaveResult(result.data, [[path, value]]);
  showToast(i18n.t("toast.saved"), "success");
  return true;
}

/** Adopts a `config.set` answer: the full snapshot when the host echoes one, the written paths otherwise. */
function adoptSaveResult(data: unknown, paths: ReadonlyArray<readonly [string, unknown]>): void {
  if (
    typeof data === "object" &&
    data !== null &&
    ("shell" in data || "theme" in data || "updates" in data)
  ) {
    adoptConfig(data);
    return;
  }

  for (const [path, value] of paths) {
    setConfigPath(config, path, value);
  }
}

async function resetSections(
  sections: ReadonlyArray<{ section: string; values: Record<string, unknown> }>,
): Promise<boolean> {
  const written: Array<readonly [string, unknown]> = [];
  for (const { section, values } of sections) {
    const result = await host.call<unknown>("config.set", configSectionParams(section, values));
    if (!result.ok) {
      notifyFailure(result);
      return false;
    }

    adoptSaveResult(
      result.data,
      Object.entries(values).map(([key, value]) => [`${section}.${key}`, value] as const),
    );
    written.push(...Object.entries(values).map(([key, value]) => [`${section}.${key}`, value] as const));
  }

  if (written.length === 0) {
    return true;
  }

  showToast(i18n.t("reset.done"), "success");
  const reload = await host.call<unknown>("shell.reload");
  if (!reload.ok && reload.reason === "failed") {
    notifyFailure(reload);
  }

  return true;
}

async function refreshShell(): Promise<void> {
  const result = await host.call<unknown>("shell.getState");
  shell = result.ok ? normalizeShellState(result.data, shell) : null;
}

/** `null` means “this build has no windows methods”, which the screens render honestly. */
async function refreshWindows(): Promise<WindowsState | null> {
  const result = await host.call<unknown>("windows.getState");
  if (!result.ok) {
    if (result.reason === "unavailable") {
      windowsState = null;
    }

    return null;
  }

  windowsState = normalizeWindowsState(result.data);
  return windowsState;
}

async function checkUpdates(): Promise<{ available: boolean; version: string | null } | null> {
  const result = await host.call<unknown>("updates.check", {
    channel,
    currentVersion: version,
  });
  if (!result.ok) {
    notifyFailure(result);
    return null;
  }

  const verdict = readUpdateCheck(result.data, version);
  if (!verdict) {
    showToast(i18n.t("overview.check.failed"), "error");
    return null;
  }

  return verdict;
}

function notifyFailure(result: Extract<CallResult<unknown>, { ok: false }>): void {
  // The host's own message is always more useful than «не удалось»; only a silent failure falls
  // back to the localised reason.
  const message = result.message.trim();
  showToast(
    message.length > 0
      ? message
      : i18n.t(result.reason === "unavailable" ? "toast.unavailable" : "toast.failed"),
    "error",
  );
}

// ---------- shell ----------

// The desktop host (neb-app) is a frameless window: its own title bar is the only one, and the
// buttons in it drive the native window through `window.*` bridge methods. The preview keeps the
// same bar so both surfaces look alike.
const hosted = typeof window !== "undefined" && "chrome" in window;
if (hosted) {
  document.documentElement.classList.add("hosted");
}

const app = document.createElement("div");
app.className = "app";

const titleBar = createTitleBar(i18n, {
  onMinimize: () => void host.call("window.minimize"),
  onClose: () => void host.call("window.close"),
  onStartDrag: () => void host.call("window.startDrag"),
  onToggleMaximize: () => void host.call("window.toggleMaximize"),
});

const ctx: AppContext = {
  i18n,
  host,
  demo: host.demo,
  config: () => config,
  shell: () => shell,
  windows: () => windowsState,
  refreshWindows,
  metrics: () => metrics,
  version: () => version,
  channel: () => channel,
  navigate: (screen) => showScreen(screen, false),
  notify: (message, kind) => showToast(message, kind ?? "info"),
  notifyFailure,
  saveConfig,
  resetSections,
  setLocalConfig: (path, value) => setConfigPath(config, path, value),
  refreshShell,
  setThemeName: (name) => {
    sidebar.setStyle(name);
    config.theme.name = name;
  },
  checkUpdates,
};

const overview = createOverview(ctx);
const appearance = createAppearanceScreen(ctx);
const profiles = createProfilesScreen(ctx);
const bar = createBarScreen(ctx);
const zapret = createZapretScreen(ctx);
const discord = createDiscordScreen(ctx);
const winmode = createWindowsModeScreen(ctx);
const windhawk = createWindhawkScreen(ctx);
const wallpapers = (() => {
  const inner = createWallpapersSection(i18n, async (method, params) => {
    const result = await host.call<unknown>(method, params);
    if (!result.ok) {
      // The section shows the host's message; a bare `null` would turn every failure into the same
      // generic sentence.
      throw new Error(result.message);
    }

    return result.data;
  });

  const root = document.createElement("section");
  root.className = "screen wallpapers-screen";
  root.dataset.screen = "wallpapers";

  const render = (): void => {
    const head = document.createElement("header");
    head.className = "screen__head";
    const copy = document.createElement("div");
    copy.className = "screen__copy";
    const title = document.createElement("h1");
    title.className = "screen__title";
    title.textContent = i18n.t("wallpapers.pageTitle");
    const subtitle = document.createElement("p");
    subtitle.className = "screen__subtitle";
    subtitle.textContent = i18n.t("wallpapers.pageSubtitle");
    copy.append(title, subtitle);
    head.append(copy);

    root.replaceChildren(head, inner.el);
    inner.render();
  };

  render();

  return {
    el: root,
    activate: () => inner.activate(),
    render,
  } satisfies Screen;
})();
const updates = createUpdatesScreen(ctx);
const account = createAccountScreen(ctx);
const logs = createLogsScreen(ctx);
const about = createAboutScreen(ctx);

const screens: Record<ScreenId, Screen> = {
  overview,
  appearance,
  profiles,
  zapret,
  discord,
  winmode,
  windhawk,
  bar,
  wallpapers: wallpapers,
  updates,
  account,
  logs,
  about,
};

let current: ScreenId | null = null;

function screenFromHash(): ScreenId {
  const raw = window.location.hash.replace(/^#\/?/, "").trim();
  return raw ? resolveScreen(raw) ?? "overview" : "overview";
}

function showScreen(id: ScreenId, moveFocus: boolean): void {
  const next = screens[id];
  if (!next) {
    return;
  }

  if (current && current !== id) {
    screens[current].deactivate?.();
  }

  for (const key of screenOrder) {
    screens[key].el.hidden = key !== id;
  }

  current = id;
  sidebar.setActive(id);
  document.title = `${i18n.t("app.name")} — ${i18n.t(screenMeta[id].label)}`;

  next.el.classList.remove("screen--enter");
  // Restart the 200 ms fade/slide when revisiting a screen.
  void next.el.offsetWidth;
  next.el.classList.add("screen--enter");

  const content = document.querySelector<HTMLElement>(".content");
  if (content) {
    content.scrollTop = 0;
  }

  void Promise.resolve(next.activate?.());

  if (moveFocus) {
    window.setTimeout(() => {
      const heading = next.el.querySelector<HTMLElement>(".screen__title");
      if (heading) {
        heading.tabIndex = -1;
        heading.focus({ preventScroll: true });
      }
    }, 0);
  }

  if (window.location.hash !== `#/${id}`) {
    window.history.replaceState(null, "", `#/${id}`);
  }
}

/** Switches the interface language. The host persists it; a failed save puts the old language back. */
async function changeLanguage(language: Language): Promise<void> {
  if (language === i18n.language) {
    return;
  }

  const previous = i18n.language;
  i18n.setLanguage(language);

  const saved = await saveConfig("language", language);
  if (!saved) {
    i18n.setLanguage(previous);
  }

  // Re-rendering the sidebar replaces the language chip; put the keyboard back on the chosen one.
  document.querySelector<HTMLElement>(`#lang-${saved ? language : previous}`)?.focus({ preventScroll: true });
}

const sidebar = createSidebar(i18n, {
  active: "overview",
  onSelect: (section) => showScreen(section, false),
  onLanguage: (language) => void changeLanguage(language),
  onStyle: () => showScreen("appearance", false),
});

const content = document.createElement("main");
content.className = "content";
content.id = "main-content";

for (const id of screenOrder) {
  screens[id].el.hidden = id !== "overview";
  content.append(screens[id].el);
}

const body = document.createElement("div");
body.className = "body";
body.append(sidebar.el, content);
app.append(titleBar.el, body);
root.append(app);

// ---------- overlays ----------

const shortcuts = createShortcuts(i18n);

function paletteEntries(): PaletteEntry[] {
  const entries: PaletteEntry[] = [];

  screenOrder.forEach((id, index) => {
    const chord = screenShortcut(index + 1, screenOrder.length);
    entries.push({
      id: `screen:${id}`,
      group: "screens",
      label: i18n.t(screenMeta[id].label),
      hint: i18n.t("palette.screen"),
      chord: chord ? `Ctrl+${chord}` : undefined,
      target: { screen: id },
    });
  });

  const onOff = (value: boolean): string =>
    i18n.t(value ? "palette.value.on" : "palette.value.off");
  const enabledModules = Object.values(config.modules).filter(Boolean).length;
  const settings: PaletteEntry[] = [
    {
      id: "setting:bar-height",
      group: "settings",
      label: i18n.t("palette.setting.barHeight"),
      hint: i18n.t("nav.appearance"),
      value: `${config.theme.barHeight} px`,
      keywords: ["height", "bar", "высота", "панель"],
      target: { screen: "appearance", selector: "#theme-bar-height" },
    },
    {
      id: "setting:dock-autohide",
      group: "settings",
      label: i18n.t("palette.setting.dockAutoHide"),
      hint: i18n.t("nav.bar"),
      value: onOff(config.shell.dockAutoHide),
      keywords: ["dock", "autohide", "док", "автоскрытие"],
      target: { screen: "bar", selector: "#bar-dock-auto-hide" },
    },
    {
      id: "setting:theme",
      group: "settings",
      label: i18n.t("palette.setting.theme"),
      hint: i18n.t("nav.appearance"),
      value: config.theme.name,
      keywords: ["theme", "style", "тема", "стиль"],
      target: { screen: "appearance", selector: "#appearance-name" },
    },
    {
      id: "setting:animations",
      group: "settings",
      label: i18n.t("palette.setting.animations"),
      hint: i18n.t("nav.appearance"),
      value: onOff(config.animations.enabled),
      keywords: ["motion", "animations", "анимации", "движение"],
      target: { screen: "appearance", selector: "#theme-animations" },
    },
    {
      id: "setting:wallpaper",
      group: "settings",
      label: i18n.t("palette.setting.wallpaper"),
      hint: i18n.t("nav.wallpapers"),
      value: onOff(config.shell.wallpaperEnabled),
      keywords: ["wallpaper", "scene", "обои", "сцена"],
      target: { screen: "wallpapers", selector: "#wallpaper-enabled" },
    },
    {
      id: "setting:winmode",
      group: "settings",
      label: i18n.t("palette.setting.winmode"),
      hint: i18n.t("nav.winmode"),
      value: windowsState ? onOff(windowsState.enabled) : i18n.t("common.na"),
      keywords: ["windows", "taskbar", "замена", "панель задач"],
      target: { screen: "winmode", selector: "#winmode-enabled" },
    },
    {
      id: "setting:modules",
      group: "settings",
      label: i18n.t("palette.setting.modules"),
      hint: i18n.t("nav.overview"),
      value: i18n.t("palette.value.modules", { count: enabledModules, total: 4 }),
      keywords: ["modules", "zapret", "discord", "windhawk", "модули"],
      target: { screen: "overview" },
    },
  ];

  return [...entries, ...settings];
}

function focusPaletteTarget(target: PaletteTarget): void {
  showScreen(target.screen, false);
  const selector = target.selector;
  if (!selector) {
    return;
  }

  const deadline = Date.now() + 2000;
  const attempt = (): void => {
    const node =
      document.querySelector<HTMLElement>(`[data-screen="${target.screen}"] ${selector}`) ??
      document.querySelector<HTMLElement>(selector);
    if (!node) {
      if (Date.now() < deadline) {
        window.setTimeout(attempt, 50);
      }

      return;
    }

    node.focus({ preventScroll: true });
    node.scrollIntoView({ block: "center" });
    node.classList.add("settings-target");
    window.setTimeout(() => node.classList.remove("settings-target"), 1600);
  };

  window.setTimeout(attempt, 50);
}

const palette = createPalette(i18n, {
  entries: paletteEntries,
  onSelect: focusPaletteTarget,
});

async function writeConfigRaw(path: string, value: unknown): Promise<boolean> {
  const result = await host.call<unknown>("config.set", configSetParams(path, value));
  if (!result.ok) {
    notifyFailure(result);
    return false;
  }

  adoptSaveResult(result.data, [[path, value]]);
  return true;
}

function onboardingSelection(): OnboardingSelection {
  return {
    wallpaper: config.shell.wallpaperEnabled,
    winmode: windowsState?.enabled ?? false,
    modules: {
      zapret: config.modules.zapret,
      discordRp: config.modules.discordRp,
      windhawk: config.modules.windhawk,
    },
  };
}

async function applyOnboarding(selection: OnboardingSelection): Promise<boolean> {
  if (selection.wallpaper !== config.shell.wallpaperEnabled) {
    const result = await host.call<unknown>("wallpaper.setSettings", {
      enabled: selection.wallpaper,
    });
    if (result.ok) {
      setConfigPath(config, "shell.wallpaperEnabled", selection.wallpaper);
    } else if (!(await writeConfigRaw("shell.wallpaperEnabled", selection.wallpaper))) {
      return false;
    }
  }

  if (selection.winmode !== (windowsState?.enabled ?? false)) {
    const toggles = windowsState?.toggles ?? windowsToggleMap(true);
    const result = await host.call<unknown>("windows.set", {
      enabled: selection.winmode,
      toggles,
    });
    if (result.ok) {
      const next = normalizeWindowsState(result.data);
      if (next) {
        windowsState = next;
      }
    } else if (!(await writeConfigRaw("windowsMode.enabled", selection.winmode))) {
      return false;
    }
  }

  const modulePatches: Array<readonly [string, boolean]> = [];
  const current = config.modules;
  if (selection.modules.zapret !== current.zapret) {
    modulePatches.push(["modules.zapret", selection.modules.zapret]);
  }

  if (selection.modules.discordRp !== current.discordRp) {
    modulePatches.push(["modules.discordRp", selection.modules.discordRp]);
  }

  if (selection.modules.windhawk !== current.windhawk) {
    modulePatches.push(["modules.windhawk", selection.modules.windhawk]);
    // The Windhawk screen edits the older spelling; keep both views of the switch in step.
    modulePatches.push(["windhawk.enabled", selection.modules.windhawk]);
  }

  for (const [path, value] of modulePatches) {
    if (!(await writeConfigRaw(path, value))) {
      return false;
    }
  }

  if (!(await writeConfigRaw("onboarded", true))) {
    return false;
  }

  config.onboarded = true;
  const reload = await host.call<unknown>("shell.reload");
  if (!reload.ok && reload.reason === "failed") {
    notifyFailure(reload);
  }

  showToast(i18n.t("onboarding.saved"), "success");
  renderAll();
  return true;
}

async function skipOnboarding(): Promise<boolean> {
  if (!(await writeConfigRaw("onboarded", true))) {
    return false;
  }

  config.onboarded = true;
  const reload = await host.call<unknown>("shell.reload");
  if (!reload.ok && reload.reason === "failed") {
    notifyFailure(reload);
  }

  showToast(i18n.t("onboarding.saved"), "success");
  return true;
}

const onboarding = createOnboarding(i18n, {
  initial: onboardingSelection,
  onApply: applyOnboarding,
  onSkip: skipOnboarding,
  onRestart: async () => {
    const result = await host.call<unknown>("app.restartShell");
    if (result.ok) {
      showToast(i18n.t("toast.restarted"), "success");
      return;
    }

    notifyFailure(result);
  },
});

document.body.append(palette.el, shortcuts.el, onboarding.el);

// ---------- redraw ----------

function renderAll(): void {
  document.documentElement.lang = i18n.language;
  applyMotion();
  document.title = `${i18n.t("app.name")} — ${i18n.t(current ? screenMeta[current].label : "nav.overview")}`;

  titleBar.render();
  sidebar.render();
  sidebar.setActive(current ?? "overview");
  palette.render();
  shortcuts.render();
  onboarding.render();

  for (const id of screenOrder) {
    screens[id].render();
  }
}

i18n.onChange(renderAll);

// ---------- host wiring ----------

host.on<unknown>("neb.metrics", (payload) => {
  metrics = readMetrics(payload);
  overview.setMetrics();
});

host.on<unknown>("neb.modules", (payload) => {
  // The push carries only what changed ({wallpapers, zapret, windhawk, discordRp, wm}); fields it
  // omits keep their previous value instead of defaulting to "running".
  const next = normalizeShellState(payload, shell);
  if (next) {
    shell = next;
    if (current === "overview") {
      overview.render();
    }
  }
});

host.on<{ section?: string }>("ui.navigate", (payload) => {
  if (typeof payload?.section === "string") {
    const target = resolveScreen(payload.section);
    if (target) {
      showScreen(target, false);
    }
  }
});

// The host owns maximise. It pushes the state after the native toggle; if the message never
// arrives the title bar keeps the maximise glyph it was drawn with.
host.on<{ maximized?: unknown }>("neb.windowState", (payload) => {
  if (typeof payload?.maximized === "boolean") {
    titleBar.setMaximized(payload.maximized);
  }
});

// ---------- keyboard ----------

function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }

  return (
    target.isContentEditable ||
    target.tagName === "INPUT" ||
    target.tagName === "TEXTAREA" ||
    target.tagName === "SELECT"
  );
}

window.addEventListener("keydown", (event) => {
  const key = event.key;

  // Esc closes the overlay in front of the page, and only that. The window itself deliberately has
  // no Esc shortcut: on a settings window an accidental Esc must never throw away the page.
  // The cancel event is dispatched so each dialog keeps its own semantics (the onboarding tour
  // persists its skip, a confirm resolves `false`); a bare dialog closes by the platform default.
  if (key === "Escape") {
    const open = Array.from(document.querySelectorAll<HTMLDialogElement>("dialog[open]"));
    const top = open.at(-1);
    if (top) {
      event.preventDefault();
      if (top.dispatchEvent(new Event("cancel", { cancelable: true }))) {
        top.close();
      }
    }

    return;
  }

  // Window chords mirror the title-bar buttons. They are handled on `keydown`, before WebView2's
  // own accelerator table sees them, so Ctrl+W cannot close the webview behind the bridge's back.
  // Chords match the physical key (`event.code`), not the letter the active layout produces: on a
  // Russian layout Ctrl+W arrives as `key: "ц"`, and a shortcut that only works in English would
  // be a bug for most of this product's users.
  if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey) {
    const chord = key.toLowerCase();
    if (event.code === "KeyW" || chord === "w") {
      event.preventDefault();
      void host.call("window.close");
      return;
    }

    if (event.code === "KeyM" || chord === "m") {
      event.preventDefault();
      if (!event.repeat) {
        void host.call("window.minimize");
      }
      return;
    }
  }

  if (key === "F11" && !event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey) {
    event.preventDefault();
    if (!event.repeat) {
      void host.call("window.toggleMaximize");
    }
    return;
  }

  const plain = !event.ctrlKey && !event.altKey && !event.metaKey;

  // Ctrl+K toggles the palette from anywhere; while it is open the same chord closes it.
  if (
    event.ctrlKey &&
    !event.altKey &&
    !event.metaKey &&
    !event.shiftKey &&
    (event.code === "KeyK" || key.toLowerCase() === "k")
  ) {
    event.preventDefault();
    if (palette.isOpen) {
      palette.close();
      return;
    }

    if (document.querySelector("dialog[open]")) {
      return;
    }

    palette.open();
    return;
  }

  // F1 works everywhere; `?` only outside a text field, where it is a real character.
  if (key === "F1" || (key === "?" && plain && !isTypingTarget(event.target))) {
    if (document.querySelector("dialog[open]")) {
      return;
    }

    event.preventDefault();
    shortcuts.open();
    return;
  }

  if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey) {
    if (document.querySelector("dialog[open]")) {
      return;
    }

    if (!/^\d$/.test(key)) {
      return;
    }

    // Ctrl+1..Ctrl+9 address the first nine sections; Ctrl+0 addresses the last one, the way a
    // browser tab strip does. With twelve sections nothing answers to the freed slots at positions
    // ten and eleven, and the sidebar simply draws no badge there.
    const digit = Number(key);
    const index = digit === 0 ? screenOrder.length : digit;
    const target = digit === 0 ? screenOrder[screenOrder.length - 1] : screenOrder[digit - 1];
    if (target && index >= 1 && index <= screenOrder.length) {
      event.preventDefault();
      showScreen(target, true);
    }
  }
});

window.addEventListener("hashchange", () => {
  const target = screenFromHash();
  if (target !== current) {
    showScreen(target, false);
  }
});

// ---------- startup ----------

renderAll();
showScreen(screenFromHash(), false);

async function handshake(): Promise<void> {
  host.send("neb.ui.ready");

  const configResult = await host.call<unknown>("config.get");
  if (configResult.ok) {
    adoptConfig(configResult.data);

    if (typeof config.language === "string") {
      i18n.setLanguage(config.language);
    }

    sidebar.setStyle(config.theme.name);
  }

  const versionResult = await host.call<unknown>("app.version");
  if (versionResult.ok) {
    const raw = versionResult.data;
    if (typeof raw === "string") {
      version = raw;
    } else if (typeof raw === "object" && raw !== null) {
      const record = raw as Record<string, unknown>;
      if (typeof record.version === "string") {
        version = record.version;
      }

      if (typeof record.channel === "string") {
        channel = record.channel;
      }
    }

    titleBar.setVersion(version);
  } else if (!host.demo) {
    titleBar.setVersion(version);
  }

  await refreshShell();
  renderAll();

  if (current === "overview") {
    void overview.activate?.();
  }

  // The preview has no host pushing metrics: emit a gentle sample so the numbers are alive.
  if (host.demo) {
    const base = host.previewMetrics();
    if (base) {
      metrics = base;
      overview.setMetrics();
    }

    window.setInterval(() => {
      const jitter = (value: number | null, spread: number, floor: number): number | null =>
        value === null ? null : Math.max(floor, value + (Math.random() - 0.5) * spread);
      const next = metrics;
      if (!next) {
        return;
      }

      metrics = {
        ...next,
        cpu: jitter(next.cpu, 2, 0),
        ram: jitter(next.ram, 1, 0),
        shell: { ...next.shell, mb: jitter(next.shell.mb, 0.6, 1) },
        wallpaper: { ...next.wallpaper, mb: jitter(next.wallpaper.mb, 0.6, 1) },
        supervisor: { ...next.supervisor, mb: jitter(next.supervisor.mb, 0.6, 1) },
        app: { ...next.app, mb: jitter(next.app.mb, 0.6, 1) },
      };
      overview.setMetrics();
    }, 4000);
  }

  if (!configLoaded) {
    console.warn("neb: config.get is unavailable, keeping the shipped defaults");
    return;
  }

  // First run only: `config.get` answered and did not report the flag. A host that cannot answer
  // at all never gets the tour, because there would be nowhere to persist the dismissal.
  if (!config.onboarded) {
    onboarding.open();
  }
}

void handshake();
