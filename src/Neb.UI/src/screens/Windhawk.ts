import type { AppContext, Screen } from "../context";
import type { CallResult } from "../host";
import type { MessageKey } from "../i18n";
import {
  button,
  el,
  emptyState,
  metaRow,
  panel,
  pill,
  setBusy,
  skeleton,
  skeletonPanel,
  switchControl,
} from "../components/controls";
import { icon } from "../components/icons";
import { confirmDialog } from "../components/Modal";
import { showToast } from "../components/toast";
import { countLabel, nouns } from "../i18n/plural";

/**
 * «Расширения (Windhawk)» — the catalog of Windhawk mods and the installed set.
 *
 * Windhawk is a separate program: ProjectNeb can ask it to install a mod, but it cannot inject
 * anything itself and it does not know more than Windhawk reports. The screen therefore keeps two
 * truths apart: the host's `windhawk.*` answers (what is installed, what Windhawk thinks is
 * enabled) and the one preference ProjectNeb owns (`windhawk.enabled`). "Состояние неизвестно" is
 * a real state, not a placeholder: Windhawk only reports a mod as enabled from its own UI.
 */

type RecordValue = Record<string, unknown>;
type Tab = "catalog" | "installed";
type ModEnabled = boolean | "unknown";

const SearchId = "windhawk-search";
const CatalogLimit = 1000;
/** The virtual list positions rows by this number, so CSS must keep `.windhawk-list__row` in sync. */
const RowHeight = 62;
const SearchDebounceMs = 260;

interface WindhawkState {
  installed: boolean;
  version: string | null;
  path: string | null;
  dataDir: string | null;
  running: boolean;
  enabled: boolean;
  autoStart: boolean;
}

interface CatalogEntry {
  id: string;
  name: string;
  installed: boolean;
  url: string | null;
}

interface Catalog {
  entries: CatalogEntry[];
  total: number;
}

interface InstalledMod {
  id: string;
  source: string | null;
  compiled: string | null;
  enabled: ModEnabled;
}

interface InstallResult {
  id: string;
  name: string;
  installed: boolean;
  enabled: string;
  needsUi: boolean;
  message: string | null;
  failed: boolean;
}

/**
 * Labels this screen owns; the shared `i18n` tables belong to another slice, so the pairs stay
 * next to the controls that use them.
 */
const copy = {
  ru: {
    chipInstalled: "Установлен",
    chipNeedsUi: "Нужен Windhawk для включения",
    chipError: "Ошибка",
    open: "Открыть Windhawk",
  },
  en: {
    chipInstalled: "Installed",
    chipNeedsUi: "Windhawk must enable it",
    chipError: "Error",
    open: "Open Windhawk",
  },
} as const;

type Busy = "module" | "open" | "refresh" | `install:${string}` | `remove:${string}` | null;

function isRecord(value: unknown): value is RecordValue {
  return typeof value === "object" && value !== null && !Array.isArray(value);
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

function text(value: unknown): string | null {
  if (typeof value === "string" && value.trim() !== "") {
    return value.trim();
  }

  if (typeof value === "number" && Number.isFinite(value)) {
    return String(value);
  }

  return null;
}

function flag(value: unknown): boolean | null {
  if (typeof value === "boolean") {
    return value;
  }

  if (typeof value === "number") {
    return value !== 0;
  }

  if (typeof value === "string") {
    const lower = value.toLowerCase();
    if (lower === "true" || lower === "yes" || lower === "on" || lower === "1") {
      return true;
    }

    if (lower === "false" || lower === "no" || lower === "off" || lower === "0") {
      return false;
    }
  }

  return null;
}

function modEnabled(value: unknown): ModEnabled {
  return flag(value) ?? "unknown";
}

/** A non-empty human message from a host field. */
function messageOf(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null;
}

/** `windhawk.getState`; a payload without any recognisable field reads as `null`. */
function readState(raw: unknown): WindhawkState | null {
  if (!isRecord(raw)) {
    return null;
  }

  const nested = pick(raw, "state", "result");
  const root = isRecord(nested) ? nested : raw;

  const looksLikeState = [
    "installed",
    "isInstalled",
    "version",
    "path",
    "dataDir",
    "data_dir",
    "running",
    "enabled",
    "autoStart",
    "auto_start",
  ].some((key) => root[key] !== undefined);

  if (!looksLikeState) {
    return null;
  }

  return {
    installed: flag(pick(root, "installed", "isInstalled")) ?? false,
    version: text(root.version),
    path: text(pick(root, "path", "installPath", "install_path")),
    dataDir: text(pick(root, "dataDir", "data_dir")),
    running: flag(pick(root, "running", "isRunning")) ?? false,
    enabled: flag(pick(root, "enabled", "moduleEnabled")) ?? false,
    autoStart: flag(pick(root, "autoStart", "auto_start")) ?? false,
  };
}

function readCatalog(raw: unknown): Catalog | null {
  const source = Array.isArray(raw) ? { entries: raw } : isRecord(raw) ? raw : null;
  if (!source) {
    return null;
  }

  const list = Array.isArray(source.entries)
    ? source.entries
    : Array.isArray(source.mods)
      ? source.mods
      : Array.isArray(source.items)
        ? source.items
        : null;

  if (!list) {
    return null;
  }

  const entries: CatalogEntry[] = [];
  const seen = new Set<string>();

  for (const item of list) {
    if (!isRecord(item)) {
      continue;
    }

    const id = text(item.id);
    if (!id || seen.has(id)) {
      continue;
    }

    seen.add(id);
    entries.push({
      id,
      name: text(pick(item, "name", "title")) ?? id,
      installed: flag(item.installed) ?? false,
      url: text(pick(item, "url", "link", "homepage")),
    });
  }

  const reported = source.total;
  const total =
    typeof reported === "number" && Number.isFinite(reported) && reported >= entries.length
      ? Math.round(reported)
      : entries.length;

  return { entries, total };
}

function readInstalled(raw: unknown): InstalledMod[] | null {
  const source = Array.isArray(raw) ? { mods: raw } : isRecord(raw) ? raw : null;
  if (!source) {
    return null;
  }

  const list = Array.isArray(source.mods)
    ? source.mods
    : Array.isArray(source.installed)
      ? source.installed
      : Array.isArray(source.items)
        ? source.items
        : null;

  if (!list) {
    return null;
  }

  const mods: InstalledMod[] = [];
  const seen = new Set<string>();

  for (const item of list) {
    if (!isRecord(item)) {
      continue;
    }

    const id = text(item.id);
    if (!id || seen.has(id)) {
      continue;
    }

    seen.add(id);
    mods.push({
      id,
      source: text(pick(item, "source", "url", "origin")),
      compiled: text(pick(item, "compiled", "compiledAt", "built")),
      enabled: modEnabled(pick(item, "enabled", "active")),
    });
  }

  return mods;
}

export function createWindhawkScreen(ctx: AppContext): Screen {
  const t = (key: MessageKey, params?: Record<string, string | number>): string =>
    ctx.i18n.t(key, params);
  const l = (key: keyof typeof copy.ru): string => copy[ctx.i18n.language][key];

  const root = document.createElement("section");
  root.className = "screen windhawk-screen";
  root.dataset.screen = "windhawk";

  let state: WindhawkState | null = null;
  let loading = true;
  let unavailable = false;
  let failed = false;
  let loadedOnce = false;

  let tab: Tab = "catalog";
  let query = "";
  let entries: CatalogEntry[] = [];
  let total = 0;
  let catalogTop = 0;
  let catalogLoading = false;
  let catalogFailed = false;
  let catalogSeq = 0;

  let mods: InstalledMod[] = [];
  let modsLoading = false;
  let modsFailed = false;
  let modsLoaded = false;

  let busy: Busy = null;
  let hint: string | null = null;
  let installResult: InstallResult | null = null;

  let queryTimer = 0;

  const enabledIndex = (): Map<string, ModEnabled> => {
    const map = new Map<string, ModEnabled>();
    for (const mod of mods) {
      map.set(mod.id, mod.enabled);
    }

    return map;
  };

  const focusByKey = (key: string): void => {
    for (const node of root.querySelectorAll<HTMLElement>("[data-focus]")) {
      if (node.dataset.focus === key) {
        node.focus({ preventScroll: true });
        return;
      }
    }
  };

  // ---------- host calls ----------

  const failToast = (result: Extract<CallResult<unknown>, { ok: false }>, key: MessageKey): void => {
    showToast(t(result.reason === "unavailable" ? "toast.unavailable" : key), "error");
  };

  const loadCatalog = async (value: string, quiet = false): Promise<void> => {
    const seq = ++catalogSeq;
    catalogLoading = true;
    catalogFailed = false;
    catalogTop = 0;
    if (!quiet) {
      render();
    }

    const trimmed = value.trim();
    const params: Record<string, unknown> = { limit: CatalogLimit };
    if (trimmed.length > 0) {
      params.query = trimmed;
    }

    const result = await ctx.host.call<unknown>("windhawk.listCatalog", params);

    if (seq !== catalogSeq) {
      // A newer query answered first: this answer belongs to a list nobody is looking at any more.
      return;
    }

    catalogLoading = false;
    if (!result.ok) {
      catalogFailed = true;
      entries = [];
      total = 0;
    } else {
      const catalog = readCatalog(result.data);
      if (!catalog) {
        catalogFailed = true;
        entries = [];
        total = 0;
      } else {
        entries = catalog.entries;
        total = catalog.total;
        catalogFailed = false;
      }
    }

    render();
  };

  const loadInstalled = async (quiet = false): Promise<void> => {
    modsLoading = true;
    modsFailed = false;
    if (!quiet) {
      render();
    }

    const result = await ctx.host.call<unknown>("windhawk.listInstalled");
    modsLoading = false;

    if (!result.ok) {
      modsFailed = true;
      mods = [];
      modsLoaded = false;
      render();
      return;
    }

    const list = readInstalled(result.data);
    if (!list) {
      modsFailed = true;
      mods = [];
      modsLoaded = false;
    } else {
      mods = list;
      modsFailed = false;
      modsLoaded = true;
    }

    render();
  };

  const load = async (quiet = false): Promise<void> => {
    if (!quiet) {
      loading = true;
      unavailable = false;
      failed = false;
      render();
    }

    const result = await ctx.host.call<unknown>("windhawk.getState");
    loading = false;
    loadedOnce = true;

    if (!result.ok) {
      state = null;
      unavailable = result.reason === "unavailable";
      failed = !unavailable;
      entries = [];
      total = 0;
      mods = [];
      modsLoaded = false;
      render();
      return;
    }

    const next = readState(result.data);
    if (!next) {
      state = null;
      unavailable = false;
      failed = true;
      render();
      return;
    }

    state = next;
    unavailable = false;
    failed = false;

    if (!state.installed) {
      entries = [];
      total = 0;
      mods = [];
      modsLoaded = false;
      modsFailed = false;
      catalogFailed = false;
      catalogLoading = false;
      modsLoading = false;
      render();
      return;
    }

    // Show the panel shells immediately, then fill them from the two list methods.
    catalogLoading = entries.length === 0;
    modsLoading = !modsLoaded;
    render();

    await Promise.all([loadCatalog(query, true), loadInstalled(true)]);
  };

  const install = async (entry: CatalogEntry): Promise<void> => {
    if (busy !== null || state?.installed !== true) {
      return;
    }

    busy = `install:${entry.id}`;
    hint = null;
    render();

    const result = await ctx.host.call<unknown>("windhawk.install", { id: entry.id });
    busy = null;

    if (!result.ok) {
      installResult = {
        id: entry.id,
        name: entry.name,
        installed: false,
        enabled: "unknown",
        needsUi: false,
        message: result.reason === "unavailable" ? null : messageOf(result.message),
        failed: true,
      };
      failToast(result, "windhawk.install.failed");
      render();
      focusByKey(`row-${entry.id}`);
      return;
    }

    const payload = isRecord(result.data) ? result.data : {};
    const needsUi = flag(payload.needsWindhawkUi) === true;
    const enabled = text(payload.enabled) ?? "unknown";
    const installed = flag(payload.installed) ?? true;

    installResult = {
      id: entry.id,
      name: entry.name,
      installed,
      enabled,
      needsUi,
      message: text(payload.message),
      failed: false,
    };

    entries = entries.map((item) =>
      item.id === entry.id ? { ...item, installed: installed } : item,
    );
    if (needsUi) {
      hint = t("windhawk.install.needsUi");
    }

    showToast(t("windhawk.install.done", { name: entry.name }), "success");
    render();
    focusByKey(`row-${entry.id}`);
    await loadInstalled(true);
  };

  const remove = async (mod: InstalledMod): Promise<void> => {
    if (busy !== null) {
      return;
    }

    const confirmed = await confirmDialog({
      title: t("windhawk.installed.removeTitle"),
      text: t("windhawk.installed.removeText", { name: mod.id }),
      confirm: t("windhawk.installed.removeConfirm"),
      cancel: t("common.cancel"),
      danger: true,
    });

    if (!confirmed) {
      return;
    }

    busy = `remove:${mod.id}`;
    render();

    const result = await ctx.host.call<unknown>("windhawk.remove", { id: mod.id });
    busy = null;

    if (!result.ok) {
      failToast(result, "windhawk.installed.removeFailed");
      render();
      focusByKey(`remove-${mod.id}`);
      return;
    }

    if (isRecord(result.data) && flag(result.data.removed) === false) {
      showToast(t("windhawk.installed.removeFailed"), "error");
      render();
      focusByKey(`remove-${mod.id}`);
      return;
    }

    mods = mods.filter((item) => item.id !== mod.id);
    entries = entries.map((item) =>
      item.id === mod.id ? { ...item, installed: false } : item,
    );
    showToast(t("windhawk.installed.removed", { name: mod.id }), "success");
    render();
    focusByKey("installed-refresh");
  };

  const openWindhawk = async (focusKey = "open-windhawk"): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "open";
    render();

    const result = await ctx.host.call<unknown>("windhawk.open");
    busy = null;

    if (!result.ok) {
      failToast(result, "windhawk.installed.openFailed");
    }

    render();
    focusByKey(focusKey);
  };

  const setModule = (next: boolean): void => {
    if (busy !== null) {
      return;
    }

    void (async () => {
      if (next) {
        // Injection is not a cosmetic setting: the explanation is shown every time the switch goes
        // on, and the persistent callout below never hides it.
        const confirmed = await confirmDialog({
          title: t("windhawk.safety.firstTitle"),
          text: t("windhawk.safety.firstText"),
          confirm: t("windhawk.safety.confirm"),
          cancel: t("windhawk.safety.cancel"),
          danger: true,
        });

        if (!confirmed) {
          focusByKey("module");
          return;
        }
      }

      busy = "module";
      render();

      const saved = await ctx.saveConfig("windhawk.enabled", next);
      busy = null;

      if (!saved) {
        showToast(t("windhawk.module.saveFailed"), "error");
        render();
        focusByKey("module");
        return;
      }

      if (state) {
        state.enabled = next;
      }

      showToast(t(next ? "windhawk.module.savedOn" : "windhawk.module.savedOff"), "success");
      render();
      focusByKey("module");
    })();
  };

  const openSite = (): void => {
    // WebView2 hands an external https link to the default browser (NavigationPolicy); in the
    // browser preview it is an ordinary new tab. `noopener` keeps the dashboard out of its opener.
    window.open("https://windhawk.net/", "_blank", "noopener,noreferrer");
  };

  const setTab = (next: Tab): void => {
    if (tab === next) {
      return;
    }

    tab = next;
    hint = null;
    render();
    focusByKey(`tab-${next}`);

    if (next === "installed" && !modsLoaded && !modsLoading) {
      void loadInstalled();
    }
  };

  // ---------- header ----------

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", t("windhawk.title")),
      el("p", "screen__subtitle", t("windhawk.subtitle")),
    );

    const aside = el("div", "screen__aside");
    if (state) {
      aside.append(
        pill(
          t(state.installed ? "windhawk.state.installed" : "windhawk.state.missing"),
          state.installed ? "on" : "warn",
        ),
      );
      if (state.installed) {
        aside.append(
          pill(
            t(state.running ? "windhawk.state.running" : "windhawk.state.stopped"),
            state.running ? "on" : "neutral",
          ),
        );
      }
    } else if (unavailable) {
      aside.append(pill(t("common.unavailable"), "warn"));
    } else if (failed) {
      aside.append(pill(t("common.failed"), "danger"));
    }

    head.append(copy, aside);
    return head;
  };

  // ---------- left column ----------

  const renderStatus = (): HTMLElement => {
    const current = state;
    const actions: HTMLElement[] = [];

    const refresh = button({
      label: t("windhawk.refresh"),
      variant: "secondary",
      size: "sm",
      icon: "refresh",
      onClick: () => void load(false),
    });
    refresh.dataset.focus = "refresh";
    refresh.disabled = busy !== null;
    if (busy === "refresh") {
      setBusy(refresh, true, t("common.loading"));
    }

    actions.push(refresh);

    const card = panel({
      title: t("windhawk.status.title"),
      icon: "extensions",
      actions,
      className: "windhawk__status",
    });

    if (current?.installed) {
      if (current.version) {
        card.body.append(metaRow(t("windhawk.version"), current.version, true));
      }

      if (current.path) {
        const path = metaRow(t("windhawk.path"), current.path, true);
        path.classList.add("windhawk__path");
        if (current.dataDir) {
          path.title = current.dataDir;
        }

        card.body.append(path);
      }

      card.body.append(
        metaRow(
          t("windhawk.runningLabel"),
          t(current.running ? "windhawk.state.running" : "windhawk.state.stopped"),
        ),
        metaRow(
          t("windhawk.autoStart"),
          t(current.autoStart ? "windhawk.autoStart.on" : "windhawk.autoStart.off"),
        ),
      );

      const open = button({
        label: t("windhawk.open"),
        variant: "secondary",
        size: "sm",
        icon: "external",
        onClick: () => void openWindhawk(),
      });
      open.dataset.focus = "open-windhawk";
      open.disabled = busy !== null;
      open.classList.add("windhawk__statusAction");
      if (busy === "open") {
        setBusy(open, true, t("windhawk.open"));
      }

      card.body.append(open);
      return card.el;
    }

    const missing = el("div", "windhawk__missing");
    const glyph = el("span", "windhawk__missingIcon");
    glyph.innerHTML = icon("extensions", 20);

    const copy = el("div", "windhawk__missingCopy");
    copy.append(
      el("h2", "windhawk__missingTitle", t("windhawk.status.missingTitle")),
      el("p", "windhawk__missingText", t("windhawk.status.missingText")),
      el("p", "windhawk__missingHint", t("windhawk.status.missingHint")),
    );

    const installButton = button({
      label: t("windhawk.install"),
      variant: "primary",
      icon: "download",
      onClick: openSite,
    });
    installButton.dataset.focus = "install-windhawk";
    copy.append(installButton);

    missing.append(glyph, copy);
    card.body.append(missing);
    return card.el;
  };

  const renderModule = (): HTMLElement => {
    const checked = state ? state.enabled : ctx.config().windhawk.enabled;
    const installMissing = state !== null && !state.installed;

    const toggle = switchControl({
      checked,
      label: t("windhawk.module.label"),
      disabled: busy !== null || state === null,
      onToggle: (next) => setModule(next),
    });
    toggle.dataset.focus = "module";

    const card = panel({
      title: t("windhawk.module.title"),
      icon: "settings",
      actions: [toggle],
      className: "windhawk__module",
    });

    card.body.append(el("p", "windhawk__moduleText", t("windhawk.module.desc")));

    const safety = el("div", "callout windhawk__safetyCallout");
    const safetyIcon = el("span", "callout__icon");
    safetyIcon.innerHTML = icon("shield", 17);
    const safetyCopy = el("div", "callout__copy");
    safetyCopy.append(
      el("span", "callout__title", t("windhawk.safety.title")),
      el("span", "callout__hint", t("windhawk.safety.text")),
    );
    safety.append(safetyIcon, safetyCopy);
    card.body.append(safety);

    if (checked && installMissing) {
      const waiting = el("div", "callout windhawk__waiting");
      const warnIcon = el("span", "callout__icon");
      warnIcon.innerHTML = icon("info", 17);
      const copy = el("div", "callout__copy");
      copy.append(
        el("span", "callout__title", t("windhawk.state.missing")),
        el("span", "callout__hint", t("windhawk.module.waiting")),
      );
      waiting.append(warnIcon, copy);
      card.body.append(waiting);
    }

    return card.el;
  };

  // ---------- catalog ----------

  const catalogRow = (entry: CatalogEntry, enabled: ModEnabled | undefined): HTMLElement => {
    const row = el("div", "windhawk-list__row");
    row.dataset.focus = `row-${entry.id}`;
    row.tabIndex = -1;
    row.setAttribute("role", "listitem");

    const glyph = el("span", "windhawk-row__icon");
    glyph.innerHTML = icon("extensions", 16);

    const copy = el("span", "windhawk-row__copy");
    copy.append(el("span", "windhawk-row__name", entry.name));
    copy.append(
      el(
        "span",
        "windhawk-row__meta",
        entry.url ? entry.url.replace(/^https?:\/\//, "") : entry.id,
      ),
    );

    const marks = el("span", "windhawk-row__marks");
    if (!entry.installed) {
      const installing = busy === `install:${entry.id}`;
      const installButton = button({
        label: t(installing ? "windhawk.installing" : "windhawk.row.install"),
        variant: "secondary",
        size: "sm",
        icon: installing ? undefined : "download",
        onClick: () => void install(entry),
      });
      installButton.dataset.focus = `install-${entry.id}`;
      installButton.disabled = busy !== null || state?.installed !== true;
      if (busy !== null && !installing) {
        installButton.title = t("windhawk.install.disabledHint");
      }

      if (installing) {
        setBusy(installButton, true, t("windhawk.installing"));
      }

      marks.append(installButton);
    } else if (enabled === true) {
      marks.append(pill(t("windhawk.mod.enabled"), "on"));
    } else if (enabled === false) {
      marks.append(pill(t("windhawk.mod.disabled"), "neutral"));
    } else if (enabled === "unknown") {
      marks.append(pill(t("windhawk.mod.unknown"), "warn"));
    } else {
      marks.append(pill(t("windhawk.installed"), "accent"));
    }

    row.append(glyph, copy, marks);
    return row;
  };

  const renderCatalogList = (): HTMLElement => {
    if (catalogLoading) {
      const loadingBox = el("div", "windhawk__listSkeleton");
      for (let index = 0; index < 6; index += 1) {
        loadingBox.append(skeleton(44, "windhawk__listSkeletonRow"));
      }

      const busyText = el("span", "windhawk__loading", t("windhawk.catalog.loading"));
      busyText.setAttribute("role", "status");
      loadingBox.append(busyText);
      return loadingBox;
    }

    if (catalogFailed) {
      const retry = button({
        label: t("common.retry"),
        variant: "secondary",
        size: "sm",
        icon: "refresh",
        onClick: () => void loadCatalog(query),
      });

      return emptyState({
        icon: "info",
        title: t("windhawk.catalog.failedTitle"),
        text: t("windhawk.catalog.failedText"),
        action: retry,
      });
    }

    if (entries.length === 0) {
      const cleared = query.trim();
      if (cleared.length > 0) {
        const reset = button({
          label: t("windhawk.search.clear"),
          variant: "secondary",
          size: "sm",
          icon: "close",
          onClick: () => {
            query = "";
            window.clearTimeout(queryTimer);
            void loadCatalog("");
            focusByKey("search");
          },
        });

        return emptyState({
          icon: "launcher",
          title: t("windhawk.catalog.emptyTitle"),
          text: t("windhawk.catalog.emptyText", { query: cleared }),
          action: reset,
        });
      }

      const retry = button({
        label: t("common.retry"),
        variant: "secondary",
        size: "sm",
        icon: "refresh",
        onClick: () => void loadCatalog(""),
      });

      return emptyState({
        icon: "extensions",
        title: t("windhawk.catalog.emptyAllTitle"),
        text: t("windhawk.catalog.emptyAllText"),
        action: retry,
      });
    }

    const enabled = enabledIndex();
    const viewport = el("div", "windhawk-list");
    viewport.tabIndex = 0;
    viewport.setAttribute("role", "list");
    viewport.setAttribute("aria-label", t("windhawk.catalog.title"));

    const spacer = el("div", "windhawk-list__spacer");
    spacer.style.height = `${entries.length * RowHeight}px`;
    spacer.setAttribute("role", "presentation");

    const layer = el("div", "windhawk-list__layer");
    layer.setAttribute("role", "presentation");
    viewport.append(spacer, layer);

    let frame = 0;
    const paint = (): void => {
      const focusKey =
        document.activeElement instanceof HTMLElement &&
        document.activeElement.classList.contains("windhawk-list__row")
          ? document.activeElement.dataset.focus
          : undefined;
      const height = viewport.clientHeight || 360;
      const top = viewport.scrollTop;
      const first = Math.max(0, Math.floor(top / RowHeight) - 2);
      const last = Math.min(
        entries.length,
        Math.ceil((top + height) / RowHeight) + 2,
      );

      const rows: HTMLElement[] = [];
      for (let index = first; index < last; index += 1) {
        const entry = entries[index];
        if (!entry) {
          continue;
        }

        const row = catalogRow(entry, enabled.get(entry.id));
        row.style.top = `${index * RowHeight}px`;
        rows.push(row);
      }

      layer.replaceChildren(...rows);
      layer.style.transform = `translateY(${first * RowHeight}px)`;

      if (focusKey) {
        const restored = layer.querySelector<HTMLElement>(
          `[data-focus="${CSS.escape(focusKey)}"]`,
        );
        restored?.focus({ preventScroll: true });
      }
    };

    viewport.addEventListener(
      "scroll",
      () => {
        catalogTop = viewport.scrollTop;
        window.cancelAnimationFrame(frame);
        frame = window.requestAnimationFrame(paint);
      },
      { passive: true },
    );

    viewport.scrollTop = catalogTop;
    paint();

    viewport.addEventListener("keydown", (event) => {
      if (
        event.key !== "ArrowDown" &&
        event.key !== "ArrowUp" &&
        event.key !== "Home" &&
        event.key !== "End"
      ) {
        return;
      }

      const rows = Array.from(layer.querySelectorAll<HTMLElement>(".windhawk-list__row"));
      if (rows.length === 0) {
        return;
      }

      event.preventDefault();
      const index = rows.indexOf(document.activeElement as HTMLElement);
      const target =
        event.key === "Home"
          ? rows[0]
          : event.key === "End"
            ? rows[rows.length - 1]
            : event.key === "ArrowDown"
              ? rows[index < 0 ? 0 : Math.min(rows.length - 1, index + 1)]
              : rows[index <= 0 ? 0 : index - 1];
      target?.focus({ preventScroll: false });
      target?.scrollIntoView({ block: "nearest" });
    });

    return viewport;
  };

  /**
   * The banner after an install: a coloured chip, the module's own message and one way out.
   *
   * Windhawk reports a freshly written source as `enabled: "unknown"`; the chip says that in
   * words instead of claiming the mod works, and the button opens Windhawk where it is enabled.
   */
  const renderInstallResult = (): HTMLElement | null => {
    if (!installResult) {
      return null;
    }

    const result = installResult;
    const needsUi =
      !result.failed && (result.needsUi || result.enabled === "unknown");
    const tone = result.failed ? "danger" : needsUi ? "warn" : "on";
    const label = result.failed
      ? l("chipError")
      : needsUi
        ? l("chipNeedsUi")
        : l("chipInstalled");

    const banner = el("div", "windhawk__result");
    banner.setAttribute("role", "status");
    banner.setAttribute("aria-live", "polite");
    const head = el("div", "windhawk__resultHead");
    head.append(
      pill(label, tone),
      el("span", "windhawk__resultName", result.name || result.id),
    );

    const open = button({
      label: l("open"),
      variant: "secondary",
      size: "sm",
      icon: "external",
      onClick: () => void openWindhawk("install-result-open"),
    });
    open.dataset.focus = "install-result-open";
    open.disabled = busy !== null;

    head.append(open);
    banner.append(head);

    if (result.failed) {
      banner.append(
        el(
          "p",
          "windhawk__resultText",
          result.message ?? t("windhawk.install.failed"),
        ),
      );
    } else if (needsUi) {
      banner.append(el("p", "windhawk__resultText", t("windhawk.install.needsUi")));
    } else if (result.message) {
      banner.append(el("p", "windhawk__resultText", result.message));
    }

    return banner;
  };

  const renderCatalog = (): HTMLElement => {
    const count =
      query.trim().length > 0
        ? t("windhawk.catalog.found", {
            count: countLabel(ctx.i18n.language, total, nouns.mods.ru, nouns.mods.en),
          })
        : t("windhawk.catalog.total", {
            total: countLabel(ctx.i18n.language, total, nouns.mods.ru, nouns.mods.en),
          });
    const actions: HTMLElement[] = [pill(count, "neutral")];
    const refresh = button({
      label: t("windhawk.refresh"),
      variant: "secondary",
      size: "sm",
      icon: "refresh",
      onClick: () => void loadCatalog(query),
    });
    refresh.dataset.focus = "catalog-refresh";
    refresh.disabled = busy !== null || catalogLoading;

    actions.push(refresh);

    const card = panel({
      title: t("windhawk.catalog.title"),
      hint: t("windhawk.catalog.hint"),
      icon: "extensions",
      actions,
      className: "windhawk__catalog",
    });

    const search = el("div", "windhawk-search");
    const glyph = el("span", "windhawk-search__icon");
    glyph.innerHTML = icon("launcher", 16);

    const input = el("input", "input windhawk-search__input");
    input.type = "search";
    input.id = SearchId;
    input.value = query;
    input.placeholder = t("windhawk.search.placeholder");
    input.setAttribute("aria-label", t("windhawk.search.label"));
    input.dataset.focus = "search";
    input.autocomplete = "off";
    input.spellcheck = false;

    const clear = el("button", "windhawk-search__clear");
    clear.type = "button";
    clear.title = t("windhawk.search.clear");
    clear.setAttribute("aria-label", t("windhawk.search.clear"));
    clear.innerHTML = icon("close", 15);
    clear.hidden = query.length === 0;
    clear.addEventListener("click", () => {
      query = "";
      input.value = "";
      clear.hidden = true;
      window.clearTimeout(queryTimer);
      void loadCatalog("");
      focusByKey("search");
    });

    const schedule = (): void => {
      query = input.value;
      clear.hidden = query.length === 0;
      window.clearTimeout(queryTimer);
      queryTimer = window.setTimeout(() => {
        void loadCatalog(query);
      }, SearchDebounceMs);
    };

    input.addEventListener("input", schedule);
    // A native type="search" clear (Esc or the platform's clear button) fires `search`, not `input`.
    input.addEventListener("search", schedule);

    search.append(glyph, input, clear);

    card.body.append(search);

    const banner = renderInstallResult();
    if (banner) {
      card.body.append(banner);
    }

    if (hint && !installResult) {
      const note = el("div", "callout callout--ok windhawk__hint");
      const noteIcon = el("span", "callout__icon");
      noteIcon.innerHTML = icon("check", 16);
      const copy = el("div", "callout__copy");
      copy.append(el("span", "callout__hint", hint));

      const open = button({
        label: t("windhawk.installed.open"),
        variant: "secondary",
        size: "sm",
        icon: "external",
        onClick: () => void openWindhawk("hint-open"),
      });
      open.dataset.focus = "hint-open";
      open.disabled = busy !== null;
      copy.append(open);

      note.append(noteIcon, copy);
      card.body.append(note);
    }

    card.body.append(renderCatalogList());

    if (entries.length > 0 && entries.length < total) {
      card.body.append(
        el(
          "p",
          "windhawk__showing",
          t("windhawk.catalog.showing", { shown: entries.length, total }),
        ),
      );
    }

    return card.el;
  };

  // ---------- installed ----------

  const renderInstalled = (): HTMLElement => {
    const actions: HTMLElement[] = [
      pill(
        countLabel(ctx.i18n.language, mods.length, nouns.mods.ru, nouns.mods.en),
        "neutral",
      ),
    ];

    const refresh = button({
      label: t("windhawk.refresh"),
      variant: "secondary",
      size: "sm",
      icon: "refresh",
      onClick: () => void loadInstalled(),
    });
    refresh.dataset.focus = "installed-refresh";
    refresh.disabled = busy !== null || modsLoading;
    actions.push(refresh);

    const card = panel({
      title: t("windhawk.installed.title"),
      hint: t("windhawk.installed.hint"),
      icon: "extensions",
      actions,
      className: "windhawk__installed",
    });

    if (modsLoading && !modsLoaded) {
      const loadingBox = el("div", "windhawk__listSkeleton");
      for (let index = 0; index < 5; index += 1) {
        loadingBox.append(skeleton(44, "windhawk__listSkeletonRow"));
      }

      const busyText = el("span", "windhawk__loading", t("windhawk.installed.loading"));
      busyText.setAttribute("role", "status");
      loadingBox.append(busyText);
      card.body.append(loadingBox);
      return card.el;
    }

    if (modsFailed) {
      const retry = button({
        label: t("common.retry"),
        variant: "secondary",
        size: "sm",
        icon: "refresh",
        onClick: () => void loadInstalled(),
      });

      card.body.append(
        emptyState({
          icon: "info",
          title: t("windhawk.installed.failedTitle"),
          text: t("windhawk.installed.failedText"),
          action: retry,
        }),
      );
      return card.el;
    }

    if (mods.length === 0) {
      const browse = button({
        label: t("windhawk.installed.emptyAction"),
        variant: "secondary",
        size: "sm",
        icon: "extensions",
        onClick: () => setTab("catalog"),
      });

      card.body.append(
        emptyState({
          icon: "extensions",
          title: t("windhawk.installed.emptyTitle"),
          text: t("windhawk.installed.emptyText"),
          action: browse,
        }),
      );
      return card.el;
    }

    const list = el("div", "windhawk__mods");
    list.setAttribute("role", "list");

    for (const mod of mods) {
      const row = el("div", "windhawk__mod");
      row.setAttribute("role", "listitem");
      row.dataset.focus = `mod-${mod.id}`;

      const glyph = el("span", "windhawk__modIcon");
      glyph.innerHTML = icon("extensions", 16);

      const copy = el("div", "windhawk__modCopy");
      copy.append(el("span", "windhawk__modName", mod.id));
      if (mod.source) {
        const source = el("span", "windhawk__modMeta", mod.source);
        source.title = mod.source;
        copy.append(source);
      }

      if (mod.compiled) {
        copy.append(
          el(
            "span",
            "windhawk__modMeta",
            `${t("windhawk.installed.compiled")}: ${mod.compiled}`,
          ),
        );
      }

      const side = el("div", "windhawk__modSide");
      if (mod.enabled === true) {
        side.append(pill(t("windhawk.mod.enabled"), "on"));
      } else if (mod.enabled === false) {
        side.append(pill(t("windhawk.mod.disabled"), "neutral"));
      } else {
        side.append(pill(t("windhawk.mod.unknown"), "warn"));

        const configure = button({
          label: t("windhawk.installed.open"),
          variant: "secondary",
          size: "sm",
          icon: "external",
          onClick: () => void openWindhawk(`open-${mod.id}`),
        });
        configure.dataset.focus = `open-${mod.id}`;
        configure.disabled = busy !== null;
        side.append(configure);
      }

      const removing = busy === `remove:${mod.id}`;
      const removeButton = button({
        label: t(removing ? "windhawk.installed.removing" : "windhawk.installed.remove"),
        variant: "danger",
        size: "sm",
        icon: "trash",
        onClick: () => void remove(mod),
      });
      removeButton.dataset.focus = `remove-${mod.id}`;
      removeButton.disabled = busy !== null;

      if (removing) {
        setBusy(removeButton, true, t("windhawk.installed.removing"));
      }

      side.append(removeButton);

      row.append(glyph, copy, side);
      list.append(row);
    }

    card.body.append(list);
    return card.el;
  };

  // ---------- tabs ----------

  const renderTabs = (): HTMLElement => {
    const tabs = el("div", "windhawk__tabs");
    tabs.setAttribute("role", "tablist");
    tabs.setAttribute("aria-label", t("windhawk.tabs.label"));

    const order: Tab[] = ["catalog", "installed"];
    for (const id of order) {
      const active = tab === id;
      const item = el("button", `windhawk__tab${active ? " windhawk__tab--active" : ""}`);
      item.type = "button";
      item.id = `windhawk-tab-${id}`;
      item.setAttribute("role", "tab");
      item.setAttribute("aria-selected", String(active));
      item.setAttribute("aria-controls", `windhawk-panel-${id}`);
      item.tabIndex = active ? 0 : -1;
      item.dataset.focus = `tab-${id}`;
      item.textContent = t(id === "catalog" ? "windhawk.tab.catalog" : "windhawk.tab.installed");
      item.addEventListener("click", () => setTab(id));
      item.addEventListener("keydown", (event) => {
        if (event.key === "Home") {
          event.preventDefault();
          setTab("catalog");
          return;
        }

        if (event.key === "End") {
          event.preventDefault();
          setTab("installed");
          return;
        }

        if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") {
          return;
        }

        event.preventDefault();
        const next: Tab = id === "catalog" ? "installed" : "catalog";
        setTab(next);
      });
      tabs.append(item);
    }

    return tabs;
  };

  // ---------- messages ----------

  const renderMessage = (
    title: string,
    body: string,
    focusKey: string,
  ): HTMLElement => {
    const card = panel({ className: "windhawk__message" });
    const note = el("div", "state-note");
    note.append(el("strong", "state-note__title", title), el("span", "state-note__text", body));

    const retry = button({
      label: t("common.retry"),
      variant: "secondary",
      size: "sm",
      icon: "refresh",
      onClick: () => void load(false),
    });
    retry.dataset.focus = focusKey;

    card.body.append(note, retry);
    return card.el;
  };

  // ---------- layout ----------

  const render = (): void => {
    const active = document.activeElement;
    const searchFocused = active instanceof HTMLInputElement && active.id === SearchId;
    const caret = searchFocused ? active.selectionStart : null;
    const focusKey =
      active instanceof HTMLElement && root.contains(active) && !searchFocused
        ? (active.dataset.focus ?? null)
        : null;

    if (loading) {
      const grid = el("div", "windhawk__grid");
      const left = el("div", "windhawk__side");
      left.append(skeletonPanel(4), skeletonPanel(3));
      grid.append(left, skeletonPanel(6));
      root.replaceChildren(renderHeader(), grid);
      return;
    }

    if (unavailable) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(
          t("windhawk.unavailable.title"),
          t("windhawk.unavailable.text"),
          "retry",
        ),
      );
      return;
    }

    if (failed || !state) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(t("windhawk.failed.title"), t("windhawk.failed.text"), "retry"),
      );
      return;
    }

    const grid = el("div", "windhawk__grid");
    const side = el("div", "windhawk__side");
    side.append(renderStatus(), renderModule());

    let right: HTMLElement;
    if (state.installed) {
      right = el("div", "windhawk__right");
      const content = tab === "catalog" ? renderCatalog() : renderInstalled();
      content.id = `windhawk-panel-${tab}`;
      content.setAttribute("role", "tabpanel");
      content.setAttribute("aria-labelledby", `windhawk-tab-${tab}`);
      right.append(renderTabs(), content);
    } else {
      right = panel({
        title: t("windhawk.catalog.title"),
        icon: "extensions",
        className: "windhawk__catalog windhawk__catalog--locked",
      }).el;

      const body = right.querySelector<HTMLElement>(".panel__body") ?? right;
      const install = button({
        label: t("windhawk.install"),
        variant: "primary",
        icon: "download",
        onClick: openSite,
      });
      install.dataset.focus = "install-windhawk-catalog";

      body.append(
        emptyState({
          icon: "extensions",
          title: t("windhawk.catalog.lockedTitle"),
          text: t("windhawk.catalog.lockedText"),
          action: install,
        }),
      );
    }

    grid.append(side, right);
    root.replaceChildren(renderHeader(), grid);

    if (searchFocused) {
      const input = root.querySelector<HTMLInputElement>(`#${SearchId}`);
      if (input) {
        input.focus({ preventScroll: true });
        if (caret !== null) {
          input.setSelectionRange(caret, caret);
        }
      }
    } else if (focusKey) {
      focusByKey(focusKey);
    }
  };

  render();

  return {
    el: root,
    async activate() {
      // The first visit paints skeletons; later visits refresh quietly so a cached list does not
      // flash away while Windhawk is being checked.
      await load(loadedOnce);
    },
    render,
  };
}
