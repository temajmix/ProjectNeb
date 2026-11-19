/**
 * The dashboard's view of the host configuration and of everything the shell reports.
 *
 * The host owns the real values; this module only knows how to describe them and how to read a
 * payload defensively. A build that answers with an older (or slightly different) shape therefore
 * still paints a working window instead of throwing: every reader below falls back to a default and
 * every unknown field is ignored.
 */

export type ScreenId =
  | "overview"
  | "appearance"
  | "zapret"
  | "discord"
  | "winmode"
  | "windhawk"
  | "bar"
  | "wallpapers"
  | "updates"
  | "account"
  | "logs"
  | "about";

/**
 * Sidebar order, and the order of the Ctrl+1..Ctrl+9 shortcuts (Ctrl+0 opens the last screen).
 *
 * «Расширения (Windhawk)» sits after «Замена Windows» and takes `Ctrl+6`, so every later section
 * shifts one place. Older slices had to choose between a pinned chord and a pinned row; here both
 * agree, because the brief fixed the chord and the row together.
 */
export const screenOrder: readonly ScreenId[] = [
  "overview",
  "appearance",
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

/**
 * The digit a screen answers to: `Ctrl+1`..`Ctrl+9` for the first nine, `Ctrl+0` for the last one.
 * With twelve sections the two screens at positions ten and eleven have no free digit, so they
 * report `null` and the sidebar simply omits their badges.
 */
export function screenShortcut(index: number, total: number): string | null {
  if (index >= 1 && index <= 9) {
    return String(index);
  }

  return index === total && total > 9 ? "0" : null;
}

export type WidgetKind = "clock" | "cpu" | "ram" | "text";

export interface BarWidget {
  id: string;
  kind: WidgetKind;
  /** A disabled widget keeps its place in the order but is not drawn on the bar. */
  enabled: boolean;
  /** Clock only. */
  format: "24" | "12";
  /** Clock only. */
  showDate: boolean;
  /** CPU/RAM only: draw the label next to the value. */
  label: boolean;
  /** Text widget only. */
  text: string;
}

export interface ThemeColors {
  background: string;
  surface: string;
  stroke: string;
  text: string;
  textDim: string;
  accent: string;
  accent2: string;
}

export interface ThemeSettings {
  id: string;
  name: string;
  colors: ThemeColors;
  /** Card corner radius in CSS pixels. */
  radius: number;
  /** Glass opacity, 0..100. */
  opacity: number;
  /** `Onest`, `JetBrains Mono` or `system`. */
  font: string;
  /** Bar height in CSS pixels. */
  barHeight: number;
  /** Dock tile edge in CSS pixels. */
  dockTileSize: number;
}

export interface ShellConfig {
  barEnabled: boolean;
  dockEnabled: boolean;
  launcherEnabled: boolean;
  wallpaperEnabled: boolean;
  hideTaskbar: boolean;
  barHeight: number;
  dockTileSize: number;
  /** The dock tucks itself away until the pointer reaches the screen edge. */
  dockAutoHide: boolean;
  widgets: BarWidget[];
}

export interface UpdatesConfig {
  channel: "stable" | "beta";
  autoUpdate: boolean;
}

/** The one switch this slice owns; the host keeps it under `windhawk.enabled`. */
export interface WindhawkConfig {
  enabled: boolean;
}

/** `animations` in `config.json`: the master switch plus the host's speed multiplier. */
export interface AnimationConfig {
  enabled: boolean;
  speed: number;
}

/** `modules` in `config.json`: which optional ProjectNeb services start with the shell. */
export interface ModulesConfig {
  wallpapers: boolean;
  zapret: boolean;
  windhawk: boolean;
  discordRp: boolean;
}

export interface AppConfig {
  shell: ShellConfig;
  theme: ThemeSettings;
  updates: UpdatesConfig;
  windhawk: WindhawkConfig;
  animations: AnimationConfig;
  modules: ModulesConfig;
  /** True once the first-run tour has been completed or skipped. */
  onboarded: boolean;
  /** `ru` or `en`; the UI normalises it before use. */
  language: string;
}

export interface ShellState {
  running: boolean;
  profile: string | null;
  modules: {
    bar: boolean;
    dock: boolean;
    launcher: boolean;
    wallpaper: boolean;
  };
}

export interface ProcessMetrics {
  pid: number | null;
  /** Working set in megabytes. */
  mb: number | null;
  running: boolean | null;
}

/**
 * `neb.metrics` as pushed by the supervisor. The reader also accepts the older flat shape
 * (`bar`, `wallpaper`, `supervisor` numbers), so a host mid-migration keeps working.
 */
export interface Metrics {
  cpu: number | null;
  ram: number | null;
  shell: ProcessMetrics;
  wallpaper: ProcessMetrics;
  supervisor: ProcessMetrics;
  app: ProcessMetrics;
}

export function emptyMetrics(): Metrics {
  return {
    cpu: null,
    ram: null,
    shell: { pid: null, mb: null, running: null },
    wallpaper: { pid: null, mb: null, running: null },
    supervisor: { pid: null, mb: null, running: null },
    app: { pid: null, mb: null, running: null },
  };
}

/** The six elements «Замена Windows» owns; the host contract fixes this list. */
export const windowsToggleIds = [
  "taskbar",
  "desktopIcons",
  "searchBox",
  "taskView",
  "widgets",
  "copilot",
] as const;

export type WindowsToggleId = (typeof windowsToggleIds)[number];

export interface WindowsState {
  enabled: boolean;
  /** What the user selected to hide (kept while the mode is off). */
  toggles: Record<WindowsToggleId, boolean>;
  /** What the host actually hid; `false` is “Не изменено”. */
  applied: Record<WindowsToggleId, boolean>;
}

export function windowsToggleMap(value: boolean): Record<WindowsToggleId, boolean> {
  return {
    taskbar: value,
    desktopIcons: value,
    searchBox: value,
    taskView: value,
    widgets: value,
    copilot: value,
  };
}

/** The shipped palette, mirroring `tokens.css`. */
export const defaultColors: ThemeColors = {
  background: "#0b0a12",
  surface: "#171225",
  stroke: "#2a2440",
  text: "#f6f4ff",
  textDim: "#b7b3c9",
  accent: "#a78bfa",
  accent2: "#7c5cff",
};

export const defaultTheme: ThemeSettings = {
  id: "liquid-glass",
  name: "Liquid Glass",
  colors: defaultColors,
  radius: 18,
  opacity: 72,
  font: "Onest",
  barHeight: 40,
  dockTileSize: 56,
};

let widgetSeq = 0;

/** A widget id that is stable inside one session and unique in the list. */
export function widgetId(): string {
  widgetSeq += 1;
  return `w${Date.now().toString(36)}${widgetSeq.toString(36)}`;
}

export function createWidget(kind: WidgetKind): BarWidget {
  return {
    id: widgetId(),
    kind,
    enabled: true,
    format: "24",
    showDate: true,
    label: false,
    text: kind === "text" ? "ProjectNeb" : "",
  };
}

export function defaultWidgets(): BarWidget[] {
  return [
    { ...createWidget("clock"), id: "clock" },
    { ...createWidget("cpu"), id: "cpu", label: true },
    { ...createWidget("ram"), id: "ram", label: true },
  ];
}

export function defaultConfig(): AppConfig {
  return {
    shell: {
      barEnabled: true,
      dockEnabled: true,
      launcherEnabled: true,
      wallpaperEnabled: true,
      hideTaskbar: true,
      barHeight: defaultTheme.barHeight,
      dockTileSize: defaultTheme.dockTileSize,
      dockAutoHide: true,
      widgets: defaultWidgets(),
    },
    theme: cloneTheme(defaultTheme),
    updates: { channel: "beta", autoUpdate: true },
    windhawk: { enabled: false },
    animations: { enabled: true, speed: 1 },
    modules: { wallpapers: false, zapret: false, windhawk: false, discordRp: false },
    onboarded: false,
    language: "ru",
  };
}

export function cloneTheme(theme: ThemeSettings): ThemeSettings {
  return {
    ...theme,
    colors: { ...theme.colors },
  };
}

export function cloneConfig(config: AppConfig): AppConfig {
  return {
    shell: {
      ...config.shell,
      widgets: config.shell.widgets.map((widget) => ({ ...widget })),
    },
    theme: cloneTheme(config.theme),
    updates: { ...config.updates },
    windhawk: { ...config.windhawk },
    animations: { ...config.animations },
    modules: { ...config.modules },
    onboarded: config.onboarded,
    language: config.language,
  };
}

// ---------- readers ----------

type RecordValue = Record<string, unknown>;

function isRecord(value: unknown): value is RecordValue {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function num(value: unknown, fallback: number): number {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }

  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) {
      return parsed;
    }
  }

  return fallback;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function text(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

const HexColour = /^#[0-9a-f]{6}$/i;

export function isHexColour(value: string): boolean {
  return HexColour.test(value);
}

function colour(value: unknown, fallback: string): string {
  return typeof value === "string" && HexColour.test(value) ? value : fallback;
}

function pick(source: RecordValue, ...keys: string[]): unknown {
  for (const key of keys) {
    const value = source[key];
    if (value !== undefined && value !== null) {
      return value;
    }
  }

  return undefined;
}

function unwrap(raw: unknown): RecordValue {
  if (!isRecord(raw)) {
    return {};
  }

  return isRecord(raw.config) ? raw.config : raw;
}

function readWidgets(raw: unknown): BarWidget[] {
  const source = Array.isArray(raw)
    ? raw
    : isRecord(raw) && Array.isArray(raw.widgets)
      ? raw.widgets
      : null;

  if (!source) {
    return defaultWidgets();
  }

  const widgets: BarWidget[] = [];
  const seen = new Set<string>();

  for (const entry of source) {
    const record = isRecord(entry) ? entry : {};
    const kind =
      typeof entry === "string"
        ? entry
        : text(pick(record, "kind", "type", "widget"), "");

    if (kind !== "clock" && kind !== "cpu" && kind !== "ram" && kind !== "text") {
      continue;
    }

    const id = text(record.id, "") || widgetId();
    if (seen.has(id)) {
      continue;
    }

    seen.add(id);
    widgets.push({
      id,
      kind,
      enabled: bool(record.enabled, true),
      format: text(record.format, "24") === "12" ? "12" : "24",
      showDate: bool(pick(record, "showDate", "date"), true),
      label: bool(record.label, kind === "cpu" || kind === "ram"),
      text: text(record.text, ""),
    });
  }

  return widgets;
}

export function normalizeTheme(raw: unknown, fallback: ThemeSettings = defaultTheme): ThemeSettings {
  const source = unwrap(raw);
  const colors = isRecord(source.colors) ? source.colors : source;

  return {
    id: text(source.id, fallback.id),
    name: text(source.name, fallback.name),
    colors: {
      background: colour(pick(colors, "background", "bg"), fallback.colors.background),
      surface: colour(pick(colors, "surface", "panel"), fallback.colors.surface),
      stroke: colour(colors.stroke, fallback.colors.stroke),
      text: colour(colors.text, fallback.colors.text),
      textDim: colour(pick(colors, "textDim", "text_dim"), fallback.colors.textDim),
      accent: colour(colors.accent, fallback.colors.accent),
      accent2: colour(pick(colors, "accent2", "accent_2"), fallback.colors.accent2),
    },
    radius: clamp(num(source.radius, fallback.radius), 0, 32),
    opacity: clamp(num(source.opacity, fallback.opacity), 0, 100),
    font: text(source.font, fallback.font),
    barHeight: clamp(num(pick(source, "barHeight", "bar_height"), fallback.barHeight), 24, 80),
    dockTileSize: clamp(
      num(pick(source, "dockTileSize", "dock_tile_size"), fallback.dockTileSize),
      32,
      96,
    ),
  };
}

export function normalizeConfig(raw: unknown): AppConfig {
  const root = unwrap(raw);
  const shell = isRecord(root.shell) ? root.shell : {};
  const theme = normalizeTheme(root.theme ?? root.appearance, defaultTheme);
  const updates = isRecord(root.updates) ? root.updates : {};
  const windhawk = isRecord(root.windhawk) ? root.windhawk : {};
  const modules = isRecord(root.modules) ? root.modules : {};
  const animations = isRecord(root.animations) ? root.animations : {};

  return {
    shell: {
      barEnabled: bool(pick(shell, "barEnabled", "bar_enabled"), true),
      dockEnabled: bool(pick(shell, "dockEnabled", "dock_enabled"), true),
      launcherEnabled: bool(pick(shell, "launcherEnabled", "launcher_enabled"), true),
      wallpaperEnabled: bool(pick(shell, "wallpaperEnabled", "wallpaper_enabled"), true),
      hideTaskbar: bool(pick(shell, "hideTaskbar", "hide_taskbar"), true),
      barHeight: clamp(
        num(pick(shell, "barHeight", "bar_height"), theme.barHeight),
        24,
        80,
      ),
      dockTileSize: clamp(
        num(pick(shell, "dockTileSize", "dock_tile_size"), theme.dockTileSize),
        32,
        96,
      ),
      dockAutoHide: bool(pick(shell, "dockAutoHide", "dock_auto_hide"), true),
      widgets: readWidgets(
        (isRecord(shell.bar) ? shell.bar.widgets : undefined) ??
          shell.widgets ??
          (root as RecordValue).barWidgets,
      ),
    },
    theme,
    updates: {
      channel: text(updates.channel, "beta") === "stable" ? "stable" : "beta",
      autoUpdate: bool(pick(updates, "autoUpdate", "auto_update"), true),
    },
    windhawk: {
      // The dashboard contract is `windhawk.enabled`; the older Rust config spells the same switch
      // `modules.windhawk`, so a host of either generation is read without inventing a value.
      enabled: bool(
        pick(windhawk, "enabled", "windhawkEnabled"),
        bool(pick(modules, "windhawk"), false),
      ),
    },
    animations: {
      enabled: bool(animations.enabled, true),
      speed: clamp(num(animations.speed, 1), 0.5, 2),
    },
    modules: {
      wallpapers: bool(modules.wallpapers, false),
      zapret: bool(modules.zapret, false),
      windhawk: bool(modules.windhawk, false),
      discordRp: bool(pick(modules, "discordRp", "discord_rp"), false),
    },
    onboarded: bool(pick(root, "onboarded", "onboardingCompleted"), false),
    language: text(root.language, "ru"),
  };
}

function moduleState(value: unknown, fallback: boolean): boolean {
  if (typeof value === "boolean") {
    return value;
  }

  if (isRecord(value)) {
    return bool(pick(value, "running", "enabled", "active"), fallback);
  }

  if (Array.isArray(value)) {
    return value.length > 0;
  }

  return fallback;
}

/**
 * Reads a shell/module snapshot. `previous` fills the fields a partial push such as `neb.modules`
 * does not repeat, so a message about wallpapers never invents a stopped bar.
 */
export function normalizeShellState(raw: unknown, previous: ShellState | null = null): ShellState | null {
  if (!isRecord(raw)) {
    return null;
  }

  const modules = isRecord(raw.modules) ? raw.modules : raw;
  const profile = raw.profile;
  const fallback = previous?.modules ?? { bar: true, dock: true, launcher: true, wallpaper: true };

  return {
    running: bool(pick(raw, "running", "shellRunning"), previous?.running ?? true),
    profile: isRecord(profile)
      ? text(profile.name, "") || null
      : typeof profile === "string"
        ? profile
        : previous?.profile ?? null,
    modules: {
      bar: moduleState(modules.bar, fallback.bar),
      dock: moduleState(modules.dock, fallback.dock),
      launcher: moduleState(modules.launcher, fallback.launcher),
      wallpaper: moduleState(
        pick(modules, "wallpaper", "wallpapers"),
        fallback.wallpaper,
      ),
    },
  };
}

/**
 * Reads `windows.getState` / `windows.set` / `windows.restore`. The host answers with
 * `{enabled, toggles, applied}`; the flat form (`{enabled, taskbar, …}`) is accepted too because the
 * slice shipped both spellings. A payload without any recognisable field reads as `null`, so the
 * screen can show “unavailable” instead of a made-up all-off state.
 */
export function normalizeWindowsState(raw: unknown): WindowsState | null {
  if (!isRecord(raw)) {
    return null;
  }

  const nested = pick(raw, "state", "result");
  const root = isRecord(nested) ? nested : raw;
  const toggles = isRecord(root.toggles) ? root.toggles : root;
  const applied = isRecord(root.applied) ? root.applied : {};

  const looksLikeState =
    typeof root.enabled === "boolean" ||
    isRecord(root.toggles) ||
    isRecord(root.applied) ||
    windowsToggleIds.some((id) => typeof root[id] === "boolean");

  if (!looksLikeState) {
    return null;
  }

  const toggleMap = {} as Record<WindowsToggleId, boolean>;
  const appliedMap = {} as Record<WindowsToggleId, boolean>;
  for (const id of windowsToggleIds) {
    toggleMap[id] = bool(toggles[id], false);
    appliedMap[id] = bool(applied[id], false);
  }

  return {
    enabled: bool(root.enabled, false),
    toggles: toggleMap,
    applied: appliedMap,
  };
}

function toMegabytes(value: number): number | null {
  if (!Number.isFinite(value) || value <= 0) {
    return null;
  }

  // A byte count is roughly six digits at the smallest plausible working set; a megabyte count is
  // three. Anything above 100 000 therefore has to be bytes, not megabytes.
  return value > 100_000 ? value / (1024 * 1024) : value;
}

const MemoryKeys = [
  "mb",
  "memoryMb",
  "memory_mb",
  "memory",
  "rssMb",
  "rss",
  "workingSetMb",
  "working_set_mb",
] as const;

/** One process record's working set: `mb`, `memoryMb`, `workingSetBytes` or plain bytes. */
function processMemory(record: RecordValue): number | null {
  for (const key of MemoryKeys) {
    const value = record[key];
    if (typeof value === "number") {
      const mb = toMegabytes(value);
      if (mb !== null) {
        return mb;
      }
    }
  }

  const bytes = pick(record, "workingSetBytes", "working_set_bytes", "bytes");
  return typeof bytes === "number" ? toMegabytes(bytes) : null;
}

function processPid(record: RecordValue): number | null {
  const value = pick(record, "pid", "processId", "process_id");
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.round(value) : null;
}

function processRunning(record: RecordValue, pid: number | null): boolean | null {
  const explicit = pick(record, "running", "alive", "active");
  if (typeof explicit === "boolean") {
    return explicit;
  }

  // A reported pid is already evidence the process exists; a record without either is unknown.
  return pid === null ? null : true;
}

function emptyProcess(): ProcessMetrics {
  return { pid: null, mb: null, running: null };
}

/**
 * Finds one process in a metrics payload. Hosts have reported metrics in several shapes while the
 * Rust app was being built, so the reader accepts a number, a `{pid, mb, running}` object, a
 * `workingSetBytes` field and a `processes: [...]` list, preferring the explicit names.
 */
function readProcess(source: RecordValue, names: readonly string[], pattern: RegExp): ProcessMetrics {
  for (const name of names) {
    const value = source[name];
    if (typeof value === "number") {
      return { pid: null, mb: toMegabytes(value), running: null };
    }

    if (isRecord(value)) {
      const pid = processPid(value);
      return {
        pid,
        mb: processMemory(value),
        running: processRunning(value, pid),
      };
    }
  }

  const processes = Array.isArray(source.processes)
    ? source.processes
    : Array.isArray(source.children)
      ? source.children
      : [];

  for (const entry of processes) {
    if (!isRecord(entry)) {
      continue;
    }

    const name = text(pick(entry, "name", "process", "exe"), "");
    if (!pattern.test(name)) {
      continue;
    }

    const pid = processPid(entry);
    return {
      pid,
      mb: processMemory(entry),
      running: processRunning(entry, pid),
    };
  }

  return emptyProcess();
}

function readPercent(source: RecordValue, names: readonly string[]): number | null {
  for (const name of names) {
    const value = source[name];
    if (typeof value === "number" && Number.isFinite(value)) {
      return value;
    }
  }

  return null;
}

export function readMetrics(payload: unknown): Metrics {
  const source = isRecord(payload) ? payload : {};

  return {
    cpu: readPercent(source, ["cpu", "cpuPercent", "cpu_percent", "cpuUsage"]),
    ram: readPercent(source, ["ram", "ramPercent", "ram_percent", "memoryPercent"]),
    shell: readProcess(
      source,
      ["shell", "bar", "panel", "nebShell", "barMemoryMb", "panelMemoryMb"],
      /(shell|bar|panel)/i,
    ),
    wallpaper: readProcess(
      source,
      ["wallpaper", "wallpapers", "scene", "wallpaperMemoryMb"],
      /wall/i,
    ),
    supervisor: readProcess(
      source,
      ["supervisor", "nebSupervisor", "neb-supervisor", "supervisorMemoryMb"],
      /supervisor|neb-app/i,
    ),
    app: readProcess(source, ["app", "nebApp", "prometheus"], /neb|project/i),
  };
}

export function formatMemory(mb: number | null, language: string, unit: string): string {
  if (mb === null) {
    return "—";
  }

  return `${new Intl.NumberFormat(language === "en" ? "en-US" : "ru-RU", {
    maximumFractionDigits: 1,
    minimumFractionDigits: 1,
  }).format(mb)} ${unit}`;
}

// ---------- config paths ----------

/** The value a dotted path points at, or `undefined`. */
export function configPathValue(config: AppConfig, path: string): unknown {
  let node: unknown = config;

  for (const segment of path.split(".")) {
    if (!isRecord(node)) {
      return undefined;
    }

    node = node[segment];
  }

  return node;
}

export function setConfigPath(config: AppConfig, path: string, value: unknown): void {
  const segments = path.split(".");
  let node: Record<string, unknown> = config as unknown as Record<string, unknown>;

  for (const segment of segments.slice(0, -1)) {
    const next = node[segment];
    if (!isRecord(next)) {
      return;
    }

    node = next;
  }

  const last = segments[segments.length - 1];
  if (last) {
    node[last] = value;
  }
}

/**
 * The params sent to `config.set`. The host may implement either a dotted path or a section patch,
 * so both spellings travel together; a strict deserialiser ignores the field it does not know.
 */
export function configSetParams(path: string, value: unknown): Record<string, unknown> {
  const segments = path.split(".");
  const section = segments[0] ?? path;
  const last = segments[segments.length - 1] ?? path;

  return {
    path,
    value,
    section,
    values: { [last]: value },
  };
}

/**
 * The params for a partial section reset. Only `section` + `values` travel: a dotted
 * `path`/`value` pair would let a host replace the whole section, and the reset must merge so the
 * keys it does not name keep their current value.
 */
export function configSectionParams(section: string, values: Record<string, unknown>): Record<string, unknown> {
  return {
    section,
    values,
  };
}
