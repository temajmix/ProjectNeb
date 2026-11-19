/**
 * A host stand-in for the plain browser preview (`npm run preview`) and for screenshots.
 *
 * Inside WebView2 `chrome.webview` exists and the real bridge is used; this module is never imported
 * into that path at runtime. It answers every method the dashboard calls with a small, consistent
 * demo dataset, so the finished window can be reviewed without a running shell. Nothing here claims
 * to be an account: the account screen stays "not connected" even in the preview.
 */

import {
  defaultConfig,
  defaultWidgets,
  emptyMetrics,
  isHexColour,
  normalizeConfig,
  normalizeTheme,
  setConfigPath,
  windowsToggleIds,
  windowsToggleMap,
  type AppConfig,
  type Metrics,
  type ThemeSettings,
  type WindowsState,
} from "./state";

type Params = Record<string, unknown>;

function isRecord(value: unknown): value is Params {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}


const demoThemes: ThemeSettings[] = [
  {
    id: "liquid-glass",
    name: "Liquid Glass",
    colors: {
      background: "#0b0a12",
      surface: "#171225",
      stroke: "#2a2440",
      text: "#f6f4ff",
      textDim: "#b7b3c9",
      accent: "#a78bfa",
      accent2: "#7c5cff",
    },
    radius: 18,
    opacity: 72,
    font: "Onest",
    barHeight: 40,
    dockTileSize: 56,
  },
  {
    id: "aurora",
    name: "Aurora",
    colors: {
      background: "#081226",
      surface: "#0e1f33",
      stroke: "#1c3a4c",
      text: "#eefcff",
      textDim: "#9fc4cf",
      accent: "#5eead4",
      accent2: "#3b82f6",
    },
    radius: 22,
    opacity: 64,
    font: "Onest",
    barHeight: 44,
    dockTileSize: 60,
  },
  {
    id: "graphite",
    name: "Graphite",
    colors: {
      background: "#0e0e11",
      surface: "#1a1a20",
      stroke: "#2e2e38",
      text: "#f4f4f6",
      textDim: "#a9a9b4",
      accent: "#d4d4dc",
      accent2: "#8b8b98",
    },
    radius: 12,
    opacity: 84,
    font: "system",
    barHeight: 36,
    dockTileSize: 52,
  },
  {
    id: "sunset",
    name: "Sunset",
    colors: {
      background: "#140b12",
      surface: "#261523",
      stroke: "#4a2739",
      text: "#fff3ee",
      textDim: "#d0a9a4",
      accent: "#f0a58f",
      accent2: "#d96a8a",
    },
    radius: 20,
    opacity: 68,
    font: "Onest",
    barHeight: 42,
    dockTileSize: 58,
  },
];

function logLines(): string[] {
  const base = Date.now() - 4 * 60_000;
  const rows: Array<[number, string, string, string]> = [
    [0, "INFO", "neb_app::window", "main window created 1100x720, caption band 40 px"],
    [120, "INFO", "neb_app::bridge", "WebView2 ready, 18 methods registered"],
    [140, "DEBUG", "neb_core::config", "config loaded from %LocalAppData%\\ProjectNeb\\config.json"],
    [210, "INFO", "neb_supervisor", "supervisor online, pid 14812"],
    [260, "INFO", "neb_shell::app", "bar attached to \\\\.\\DISPLAY1, 40 px"],
    [430, "INFO", "neb_shell::dock", "6 pinned, 4 running, 10 tiles at 1920x1080"],
    [510, "INFO", "neb_wallpaper", "worker attached to WorkerW, scene \u00abaurora\u00bb"],
    [640, "INFO", "neb_shell::launcher", "index ready: 812 shortcuts in 194 ms"],
    [820, "WARN", "neb_shell::dock", "icon cache miss for Discord.exe, extracted in 12 ms"],
    [1_050, "INFO", "neb_app::updates", "channel beta, last check 2026-11-18T18:31:06Z"],
    [1_320, "DEBUG", "neb_core::metrics", "bar 28.4 MB, wallpaper 96.2 MB, supervisor 18.7 MB"],
    [1_470, "INFO", "neb_app::commands", "config.set shell.bar.widgets (3 items)"],
    [1_900, "INFO", "neb_shell::bar", "theme \u00abLiquid Glass\u00bb applied in 38 ms"],
    [2_040, "DEBUG", "neb_shell::hotkeys", "Alt+Space registered"],
    [2_210, "INFO", "neb_wallpaper::d3d", "swapchain 1920x1080, 60 Hz, HDR off"],
    [2_660, "WARN", "neb_supervisor::watch", "wallpaper heartbeat late by 420 ms"],
    [2_900, "INFO", "neb_supervisor::watch", "wallpaper heartbeat restored"],
    [3_120, "INFO", "neb_shell::bar", "monitor change: 1 -> 1, layout kept"],
    [3_480, "DEBUG", "neb_core::metrics", "sample cpu 7 %, ram 61 %"],
    [3_910, "INFO", "neb_app::commands", "themes.save \u00abliquid-glass\u00bb"],
    [4_060, "INFO", "neb_app::commands", "shell.reload requested"],
    [4_240, "INFO", "neb_shell::app", "bar reloaded in 41 ms"],
    [4_600, "ERROR", "neb_shell::launcher", "hotkey Ctrl+Alt+Space is taken by another process"],
    [4_640, "INFO", "neb_shell::launcher", "falling back to Alt+Space"],
    [4_900, "DEBUG", "neb_core::ipc", "ping 0.4 ms, 3 clients"],
  ];

  return rows.map(([offset, level, target, message]) => {
    const stamp = new Date(base + offset).toISOString().replace("T", " ").replace("Z", "");
    return `${stamp}  ${level.padEnd(5)} ${target}: ${message}`;
  });
}

export interface DemoHost {
  invoke<T>(method: string, params?: unknown): Promise<T>;
  metrics(): Metrics;
}

/**
 * The browser preview can start in either zapret state so both are reviewable without a host:
 * `?zapret=installed` or `localStorage["neb.demo.zapret"] = "installed"`. Without either the pack is
 * reported missing, which is the honest default of a fresh machine.
 */
function demoZapretInstalled(): boolean {
  if (typeof window === "undefined") {
    return false;
  }

  try {
    const query = new URLSearchParams(window.location.search).get("zapret");
    if (query) {
      return query === "installed";
    }

    return window.localStorage.getItem("neb.demo.zapret") === "installed";
  } catch {
    return false;
  }
}

/**
 * The browser preview starts with Windhawk missing, which is the honest state of a fresh machine.
 * `?windhawk=installed` (or `localStorage["neb.demo.windhawk"] = "installed"`) makes the catalog and
 * the installed list reviewable.
 */
function demoWindhawkInstalled(): boolean {
  if (typeof window === "undefined") {
    return false;
  }

  try {
    const query = new URLSearchParams(window.location.search).get("windhawk");
    if (query) {
      return query === "installed";
    }

    return window.localStorage.getItem("neb.demo.windhawk") === "installed";
  } catch {
    return false;
  }
}

/** Twenty representative catalog entries; the real catalog has hundreds and the UI virtualises it. */
function demoWindhawkCatalog(): Array<{ id: string; name: string; url: string; installed: boolean }> {
  const rows: Array<[string, string]> = [
    ["taskbar-clock", "Taskbar Clock Customization"],
    ["taskbar-autohide", "Taskbar auto-hide when maximized"],
    ["explorer-frame", "Translucent Windows"],
    ["taskbar-icon-size", "Taskbar icon size tuner"],
    ["taskbar-labels", "Taskbar labels for Windows 11"],
    ["taskbar-volume", "Taskbar volume control"],
    ["window-corner-radius", "Window corner radius"],
    ["dark-context-menus", "Dark context menus"],
    ["start-menu-blur", "Start menu blur"],
    ["alt-tab-tweaks", "Alt+Tab tweaks"],
    ["explorer-tabs", "Explorer tabs"],
    ["tray-icon-spacing", "Tray icon spacing"],
    ["window-drag-snap", "Drag snap improvements"],
    ["taskbar-clock-seconds", "Clock with seconds"],
    ["notification-center", "Notification center tweaks"],
    ["taskbar-thumbnails", "Taskbar thumbnails"],
    ["immersive-scrollbars", "Immersive scrollbars"],
    ["keyboard-layout-indicator", "Keyboard layout indicator"],
    ["hide-taskbar-labels", "Hide taskbar labels"],
    ["start-all-apps", "All apps in Start"],
  ];

  return rows.map(([id, name]) => ({
    id,
    name,
    url: `https://windhawk.net/mods/${id}`,
    installed: false,
  }));
}

function demoZapretChecks(running: boolean): Array<Record<string, unknown>> {
  return [
    {
      id: "windivert",
      name: "WinDivert",
      ok: true,
      hint: "Драйвер 2.2.2 установлен, подпись действительна.",
    },
    {
      id: "winws",
      name: "winws",
      ok: running,
      hint: running ? "Процесс работает, PID 9124." : "Процесс не запущен: обход выключен.",
    },
    {
      id: "proxy",
      name: "Системный прокси",
      ok: true,
      hint: "Прокси не перехвачен, настройки Windows не изменены.",
    },
    {
      id: "hosts",
      name: "hosts",
      ok: false,
      hint: "Найдено 3 записи, добавленные не ProjectNeb: обновите пакет.",
    },
  ];
}

function demoDiscordPresets(): Array<Record<string, unknown>> {
  return [
    {
      id: "main",
      name: "ProjectNeb",
      details: "Кастомизирую Windows",
      state: "ProjectNeb",
      largeImageKey: "neb",
      largeImageText: "ProjectNeb",
      smallImageKey: "beta",
      smallImageText: "beta",
      buttons: [{ label: "GitHub", url: "https://github.com/temajmix/ProjectNeb" }],
      startTimestamp: true,
    },
    {
      id: "work",
      name: "Работа",
      details: "В редакторе",
      state: "Пишу код",
      largeImageKey: "neb",
      largeImageText: "ProjectNeb",
      smallImageKey: "code",
      smallImageText: "Rust",
      buttons: [],
      startTimestamp: false,
    },
    {
      id: "music",
      name: "Музыка",
      details: "Слушаю плейлист",
      state: "Aurora",
      largeImageKey: "neb",
      largeImageText: "ProjectNeb",
      smallImageKey: "",
      smallImageText: "",
      buttons: [{ label: "Сцена", url: "https://github.com/temajmix/ProjectNeb" }],
      startTimestamp: true,
    },
  ];
}

function demoOnboardingForced(): boolean {
  if (typeof window === "undefined") {
    return false;
  }

  const query = new URLSearchParams(window.location.search);
  return query.get("onboarding") === "1" || query.get("onboard") === "1";
}

function demoDiscordRules(): Array<Record<string, unknown>> {
  return [
    { id: "rule-chrome", match: "exe", pattern: "chrome.exe", presetId: "main" },
    { id: "rule-code", match: "title", pattern: "Visual Studio Code", presetId: "work" },
    { id: "rule-spotify", match: "class", pattern: "SpotifyMainWindow", presetId: "music" },
  ];
}

export function createDemoHost(): DemoHost {
  const config: AppConfig = defaultConfig();
  config.theme = normalizeTheme(demoThemes[0], config.theme);
  config.theme.name = "Liquid Glass";
  config.shell.widgets = defaultWidgets();
  // The preview is a review surface, not a fresh install: keep the tour out of the way unless a
  // screenshot explicitly asks for it.
  config.onboarded = !demoOnboardingForced();

  const themes: ThemeSettings[] = demoThemes.map((theme) => normalizeTheme(theme, theme));
  let activeTheme = config.theme.id;
  let wallpaper = {
    enabled: true,
    kind: "Web",
    path: null as string | null,
    color: "#141021",
    playlist: [] as string[],
    rotateMinutes: 30,
    pauseOnFullscreen: true,
    pauseOnBattery: true,
    running: true,
  };
  let logs: string[] | null = null;
  const version = "0.1.2";

  // The demo starts with the replacement on and one element deliberately untouched, so both badge
  // states are visible in a review screenshot.
  let windowsMode: WindowsState = {
    enabled: true,
    toggles: {
      taskbar: true,
      desktopIcons: true,
      searchBox: true,
      taskView: true,
      widgets: true,
      copilot: false,
    },
    applied: {
      taskbar: true,
      desktopIcons: true,
      searchBox: true,
      taskView: true,
      widgets: true,
      copilot: false,
    },
  };

  let zapretInstalled = demoZapretInstalled();
  let zapretRunning = false;
  let zapretStrategy: string | null = null;
  let zapretError: string | null = zapretInstalled
    ? "Прошлый запуск завершился с кодом 1: не найден WinDivert."
    : null;

  const zapretStrategies = [
    {
      id: "general",
      name: "General",
      description: "Стандартный набор фильтров для большинства провайдеров.",
    },
    {
      id: "alt",
      name: "ALT",
      description: "Другая последовательность обхода для мобильных сетей.",
    },
    {
      id: "fake",
      name: "Fake TLS",
      description: "Подмена TLS-рукопожатия без прокси и сторонних служб.",
    },
    {
      id: "simple",
      name: "Simple Fake",
      description: "Лёгкий режим для слабых роутеров и Wi-Fi.",
    },
  ];

  const zapretStatus = (): Record<string, unknown> =>
    zapretInstalled
      ? {
          installed: true,
          version: "0.41.2",
          path: "%LocalAppData%\\ProjectNeb\\zapret",
          strategy: zapretStrategy,
          running: zapretRunning,
          lastError: zapretError,
          requiresAdmin: true,
          elevated: false,
          uacPath: "%ProgramFiles%\\ProjectNeb\\zapret\\winws.exe",
        }
      : {
          installed: false,
          version: null,
          path: null,
          strategy: null,
          running: false,
          lastError: null,
        };

  let discordStatus: "off" | "connected" | "error" = "off";
  let discordAppId = "";
  let discordError: string | null = null;
  let discordPresets = demoDiscordPresets();
  let discordActive = "main";
  let discordRules = demoDiscordRules();

  const windhawkInstalled = demoWindhawkInstalled();
  const windhawkCatalog = demoWindhawkCatalog();
  const windhawkInitialIds = ["taskbar-clock", "taskbar-autohide", "explorer-frame"];
  for (const entry of windhawkCatalog) {
    entry.installed = windhawkInstalled && windhawkInitialIds.includes(entry.id);
  }

  let windhawkMods: Array<Record<string, unknown>> = windhawkInstalled
    ? [
        {
          id: "taskbar-clock",
          source: "https://windhawk.net/mods/taskbar-clock",
          compiled: "17.11.2026 21:04",
          enabled: true,
        },
        {
          id: "taskbar-autohide",
          source: "https://windhawk.net/mods/taskbar-autohide",
          compiled: "09.11.2026 14:32",
          enabled: false,
        },
        {
          id: "explorer-frame",
          source: "https://windhawk.net/mods/explorer-frame",
          compiled: "03.11.2026 08:17",
          enabled: "unknown",
        },
      ]
    : [];

  const windhawkStatus = (): Record<string, unknown> =>
    windhawkInstalled
      ? {
          installed: true,
          version: "1.6.1",
          path: "%ProgramFiles%\\Windhawk",
          dataDir: "%LocalAppData%\\Windhawk",
          running: true,
          enabled: config.windhawk.enabled,
          autoStart: true,
        }
      : {
          installed: false,
          version: null,
          path: null,
          dataDir: null,
          running: false,
          enabled: config.windhawk.enabled,
          autoStart: false,
        };

  const answer = async <T>(value: T): Promise<T> => clone(value);

  return {
    metrics() {
      return {
        ...emptyMetrics(),
        cpu: 7,
        ram: 61,
        shell: { pid: 14812, mb: 28.4, running: true },
        wallpaper: { pid: 14920, mb: 96.2, running: true },
        supervisor: { pid: 14808, mb: 18.7, running: true },
        app: { pid: 14790, mb: 42.6, running: true },
      };
    },

    async invoke<T>(method: string, params?: unknown): Promise<T> {
      const input = isRecord(params) ? params : {};

      switch (method) {
        case "config.get":
          return answer(normalizeConfig(config)) as Promise<T>;

        case "config.set": {
          const path = typeof input.path === "string" ? input.path : "";
          if (path.length > 0 && input.value !== undefined) {
            setConfigPath(config, path, clone(input.value));
          } else if (typeof input.section === "string" && isRecord(input.values)) {
            for (const [key, value] of Object.entries(input.values)) {
              setConfigPath(config, `${input.section}.${key}`, clone(value));
            }
          }

          return answer(normalizeConfig(config)) as Promise<T>;
        }

        case "themes.list":
          return answer({
            themes: themes.map((theme) => ({ ...theme, colors: { ...theme.colors } })),
            active: activeTheme,
          }) as Promise<T>;

        case "themes.save": {
          const source = isRecord(input.theme) ? input.theme : input;
          const next = normalizeTheme(source, config.theme);
          const existing = themes.findIndex((theme) => theme.id === next.id);
          if (existing >= 0) {
            themes[existing] = next;
          } else {
            themes.push(next);
          }

          config.theme = clone(next);
          activeTheme = next.id;
          return answer({ theme: next, themes }) as Promise<T>;
        }

        case "shell.getState":
          return answer({
            running: true,
            profile: { id: "main", name: "Основной" },
            modules: {
              bar: config.shell.barEnabled,
              dock: config.shell.dockEnabled,
              launcher: config.shell.launcherEnabled,
              wallpaper: config.shell.wallpaperEnabled,
            },
          }) as Promise<T>;

        case "shell.reload":
        case "shell.restart":
        case "app.restartShell":
          return answer({ ok: true, running: true }) as Promise<T>;

        case "windows.getState":
          return answer(windowsMode) as Promise<T>;

        case "windows.set": {
          const source = isRecord(input.toggles) ? input.toggles : input;
          const next = { ...windowsMode.toggles };
          for (const id of windowsToggleIds) {
            if (typeof source[id] === "boolean") {
              next[id] = source[id] as boolean;
            }
          }

          // The host turns the mode on when a toggle is selected without an explicit `enabled`.
          const enabled =
            typeof input.enabled === "boolean"
              ? input.enabled
              : Object.values(next).some(Boolean);

          windowsMode = {
            enabled,
            toggles: next,
            applied: enabled ? { ...next } : windowsToggleMap(false),
          };
          return answer(windowsMode) as Promise<T>;
        }

        case "windows.restore":
          windowsMode = {
            enabled: false,
            toggles: windowsToggleMap(false),
            applied: windowsToggleMap(false),
          };
          return answer(windowsMode) as Promise<T>;

        case "app.version":
          return answer({ version, channel: config.updates.channel }) as Promise<T>;

        case "app.openChangelog":
        case "app.openLogsFolder":
        case "window.minimize":
        case "window.close":
          return answer({ ok: true }) as Promise<T>;

        case "app.quit":
          return answer({ ok: true }) as Promise<T>;

        case "updates.check":
          return answer({
            ok: true,
            available: false,
            current: version,
            checkedAt: new Date().toISOString(),
          }) as Promise<T>;

        case "updates.whatsNew":
          return answer({
            version,
            notes: [
              "Главное окно на WebView2: обзор, тема, панель, обои, обновления, логи.",
              "Виджеты панели переставляются мышью и сохраняются в config.json.",
              "Живые обои: сцены Web и Video, пауза в полноэкранных приложениях.",
              "Исправлено: курсор не залипает на панели и доке после перетаскивания.",
            ],
          }) as Promise<T>;

        case "wallpaper.getSettings":
          return answer(wallpaper) as Promise<T>;

        case "wallpaper.setSettings":
          wallpaper = { ...wallpaper, ...(input as Partial<typeof wallpaper>) };
          return answer(wallpaper) as Promise<T>;

        case "wallpaper.restart":
          return answer({ ok: true, running: wallpaper.running }) as Promise<T>;

        case "wallpaper.pickFile":
          return answer({ path: null }) as Promise<T>;

        case "account.getState":
        case "account.signOut":
          return answer({
            signedIn: false,
            plan: "free",
            until: null,
            name: null,
            avatar: null,
            stale: false,
            configured: false,
          }) as Promise<T>;

        case "account.signIn":
        case "account.refresh":
          throw new Error("ProjectNeb Pro is not configured in the preview");

        case "license.getState":
          return answer({
            signedIn: false,
            plan: "free",
            until: null,
            stale: false,
            valid: false,
          }) as Promise<T>;

        case "logs.tail": {
          logs ??= logLines();
          return answer({ lines: logs, path: "%LocalAppData%\\ProjectNeb\\logs" }) as Promise<T>;
        }

        case "themes.remove":
          return answer({ ok: true }) as Promise<T>;

        case "settings.setLanguage":
          if (typeof input.language === "string") {
            config.language = input.language;
          }
          return answer({ ok: true }) as Promise<T>;

        case "zapret.status":
          return answer(zapretStatus()) as Promise<T>;

        case "zapret.listStrategies":
          return answer({
            strategies: zapretInstalled ? zapretStrategies : [],
            selected: zapretStrategy,
          }) as Promise<T>;

        case "zapret.import": {
          const path = typeof input.path === "string" ? input.path.trim() : "";
          if (path.length === 0) {
            throw new Error("zapret.import: the path is empty");
          }

          zapretInstalled = true;
          zapretStrategy = zapretStrategy ?? zapretStrategies[0]?.id ?? null;
          zapretError = null;
          return answer(zapretStatus()) as Promise<T>;
        }

        case "zapret.start": {
          if (!zapretInstalled) {
            throw new Error("zapret.start: the pack is not installed");
          }

          const id = typeof input.strategy === "string" ? input.strategy : "";
          const known = zapretStrategies.some((strategy) => strategy.id === id);
          if (!known) {
            throw new Error(`zapret.start: unknown strategy '${id}'`);
          }

          zapretStrategy = id;
          zapretRunning = true;
          zapretError = null;
          return answer(zapretStatus()) as Promise<T>;
        }

        case "zapret.stop":
          zapretRunning = false;
          zapretError = null;
          return answer(zapretStatus()) as Promise<T>;

        case "zapret.selfTest":
          return answer({ checks: demoZapretChecks(zapretRunning) }) as Promise<T>;

        case "discord.getState":
          return answer({
            status: discordStatus,
            appId: discordAppId,
            error: discordError,
            presets: discordPresets,
            activePresetId: discordActive,
          }) as Promise<T>;

        case "discord.getRules":
          return answer({ rules: discordRules }) as Promise<T>;

        case "discord.setRules": {
          const list = Array.isArray(input.rules) ? input.rules : null;
          if (!list) {
            throw new Error("discord.setRules: 'rules' must be a list");
          }

          discordRules = list
            .filter((entry) => isRecord(entry))
            .map((entry, index) => ({
              id:
                typeof entry.id === "string" && entry.id.length > 0
                  ? entry.id
                  : `rule-${index + 1}`,
              match: entry.match,
              pattern: entry.pattern,
              presetId: entry.presetId,
            }));
          return answer({ rules: discordRules }) as Promise<T>;
        }

        case "discord.connect": {
          const appId = typeof input.appId === "string" ? input.appId.trim() : "";
          if (!/^\d{5,}$/.test(appId)) {
            throw new Error("discord.connect: 'appId' must be a snowflake");
          }

          discordAppId = appId;
          discordStatus = "connected";
          discordError = null;
          return answer({ ok: true, status: discordStatus, appId }) as Promise<T>;
        }

        case "discord.disconnect":
          discordStatus = "off";
          discordError = null;
          return answer({ ok: true, status: discordStatus }) as Promise<T>;

        case "discord.setPreset": {
          const id = typeof input.id === "string" ? input.id : "";
          if (!discordPresets.some((preset) => preset.id === id)) {
            throw new Error(`discord.setPreset: unknown preset '${id}'`);
          }

          discordActive = id;
          return answer({ ok: true, activePresetId: id }) as Promise<T>;
        }

        case "discord.savePreset": {
          if (input.delete === true) {
            const id = typeof input.id === "string" ? input.id : "";
            discordPresets = discordPresets.filter((preset) => preset.id !== id);
            if (discordActive === id) {
              discordActive =
                typeof discordPresets[0]?.id === "string" ? (discordPresets[0].id as string) : "";
            }

            return answer({ ok: true, presets: discordPresets }) as Promise<T>;
          }

          const source = isRecord(input.preset) ? input.preset : input;
          const name = typeof source.name === "string" ? source.name.trim() : "";
          if (name.length === 0) {
            throw new Error("discord.savePreset: a preset needs a name");
          }

          const id =
            typeof source.id === "string" && source.id.length > 0
              ? source.id
              : `p${discordPresets.length + 1}`;
          const preset = {
            id,
            name,
            details: typeof source.details === "string" ? source.details : "",
            state: typeof source.state === "string" ? source.state : "",
            largeImageKey: typeof source.largeImageKey === "string" ? source.largeImageKey : "",
            largeImageText: typeof source.largeImageText === "string" ? source.largeImageText : "",
            smallImageKey: typeof source.smallImageKey === "string" ? source.smallImageKey : "",
            smallImageText: typeof source.smallImageText === "string" ? source.smallImageText : "",
            buttons: Array.isArray(source.buttons) ? source.buttons : [],
            startTimestamp: source.startTimestamp === true,
          };

          const index = discordPresets.findIndex((entry) => entry.id === id);
          if (index >= 0) {
            discordPresets[index] = preset;
          } else {
            discordPresets = [...discordPresets, preset];
          }

          return answer({ preset, presets: discordPresets }) as Promise<T>;
        }

        case "windhawk.getState":
          return answer(windhawkStatus()) as Promise<T>;

        case "windhawk.listCatalog": {
          const needle =
            typeof input.query === "string" ? input.query.trim().toLowerCase() : "";
          const limit =
            typeof input.limit === "number" && Number.isFinite(input.limit) && input.limit > 0
              ? Math.min(Math.floor(input.limit), 946)
              : 100;

          const matching = windhawkCatalog.filter(
            (entry) =>
              needle.length === 0 ||
              entry.name.toLowerCase().includes(needle) ||
              entry.id.toLowerCase().includes(needle),
          );

          return answer({
            entries: matching.slice(0, limit).map((entry) => ({ ...entry })),
            total: matching.length,
          }) as Promise<T>;
        }

        case "windhawk.listInstalled":
          return answer({ mods: windhawkMods }) as Promise<T>;

        case "windhawk.install": {
          if (!windhawkInstalled) {
            throw new Error("windhawk.install: Windhawk is not installed");
          }

          const id = typeof input.id === "string" ? input.id : "";
          const entry = windhawkCatalog.find((item) => item.id === id);
          if (!entry) {
            throw new Error(`windhawk.install: unknown mod '${id}'`);
          }

          entry.installed = true;
          if (!windhawkMods.some((mod) => mod.id === id)) {
            windhawkMods = [
              ...windhawkMods,
              {
                id,
                source: entry.url,
                compiled: new Date().toISOString().slice(0, 16).replace("T", " "),
                enabled: "unknown",
              },
            ];
          }

          // The demo reports “unknown”, the state a real Windhawk shows until its own UI is opened.
          return answer({
            installed: true,
            enabled: "unknown",
            needsWindhawkUi: true,
          }) as Promise<T>;
        }

        case "windhawk.remove": {
          const id = typeof input.id === "string" ? input.id : "";
          const entry = windhawkCatalog.find((item) => item.id === id);
          const before = windhawkMods.length;
          windhawkMods = windhawkMods.filter((mod) => mod.id !== id);
          if (entry) {
            entry.installed = false;
          }

          return answer({ removed: windhawkMods.length < before }) as Promise<T>;
        }

        case "windhawk.open":
          return answer({ ok: true }) as Promise<T>;

        default:
          throw new Error(`unknown method '${method}'`);
      }
    },
  };
}

/** The swatch a preset list shows next to each theme name. */
export function themeSwatch(theme: ThemeSettings): string {
  const accent = isHexColour(theme.colors.accent) ? theme.colors.accent : "#a78bfa";
  const background = isHexColour(theme.colors.background) ? theme.colors.background : "#0b0a12";
  return `radial-gradient(circle at 32% 26%, ${accent} 0 34%, ${background} 100%)`;
}
