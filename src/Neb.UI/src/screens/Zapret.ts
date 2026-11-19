import type { AppContext, Screen } from "../context";
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
} from "../components/controls";
import { icon } from "../components/icons";
import { showToast } from "../components/toast";

/**
 * «Сеть и доступ» — the zapret pack.
 *
 * The host owns every fact on this screen: whether the pack is installed, which strategy is running
 * and what the self-test found. A build that does not implement the `zapret.*` methods answers with
 * an error and the screen paints an inline «Недоступно в этой сборке» instead of guessing, so a
 * missing host method can never turn into a fake "installed" badge.
 */

type RecordValue = Record<string, unknown>;

interface Strategy {
  id: string;
  name: string;
  description: string | null;
}

interface AdminInfo {
  path: string;
  elevated: boolean;
  required: boolean;
}

interface PackStatus {
  installed: boolean;
  version: string | null;
  path: string | null;
  strategy: string | null;
  running: boolean;
  lastError: string | null;
  admin: AdminInfo | null;
}

interface Check {
  id: string;
  name: string;
  hint: string | null;
  ok: boolean;
}

type Busy = "start" | "stop" | "import" | "pick" | "detect" | null;

/**
 * Labels the three module screens own.
 *
 * `MessageKey` is typed from the shared `i18n` tables, and those are owned by another slice, so the
 * new controls keep their Russian and English pairs here instead of widening a file this branch
 * must not touch.
 */
const copy = {
  ru: {
    pick: "Выбрать папку…",
    picking: "Выбираю папку…",
    detect: "Найти автоматически",
    detecting: "Ищу пакет…",
    detected: "Найденный пакет",
    detectMiss: "Папка zapret не найдена. Выберите её вручную.",
    manual: "Указать вручную",
    manualHide: "Скрыть ручной ввод",
  },
  en: {
    pick: "Choose a folder…",
    picking: "Choosing a folder…",
    detect: "Find automatically",
    detecting: "Searching…",
    detected: "Detected pack",
    detectMiss: "No zapret folder was found. Choose it manually.",
    manual: "Enter a path manually",
    manualHide: "Hide the manual field",
  },
} as const;

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

  if (typeof value === "string") {
    const lower = value.toLowerCase();
    if (lower === "true" || lower === "yes" || lower === "on") {
      return true;
    }

    if (lower === "false" || lower === "no" || lower === "off") {
      return false;
    }
  }

  return null;
}

function entryId(entry: unknown): string | null {
  if (typeof entry === "string") {
    return text(entry);
  }

  if (!isRecord(entry)) {
    return null;
  }

  return (
    text(pick(entry, "id", "key", "value", "strategy", "name")) ??
    (isRecord(entry.strategy) ? text(pick(entry.strategy, "id", "name")) : null)
  );
}

function readAdmin(raw: RecordValue): AdminInfo | null {
  const uacRaw = pick(raw, "uacPath", "uac_path", "adminPath");
  const direct = text(uacRaw);
  const node = pick(raw, "admin", "elevation", "uac");

  if (isRecord(node)) {
    const path = text(pick(node, "path", "uacPath", "executable", "exe")) ?? direct ?? "";
    const elevated = flag(pick(node, "elevated", "isElevated", "admin")) ?? false;
    const required = flag(pick(node, "required", "needsAdmin", "requiresAdmin")) ?? true;
    return path.length > 0 || elevated ? { path, elevated, required } : null;
  }

  if (typeof node === "string" && node.trim() !== "") {
    return { path: node.trim(), elevated: false, required: true };
  }

  if (direct) {
    return { path: direct, elevated: flag(pick(raw, "elevated", "isElevated")) ?? false, required: true };
  }

  const required = flag(pick(raw, "requiresAdmin", "needsAdmin", "adminRequired"));
  if (required === true) {
    return { path: "", elevated: flag(raw.elevated) ?? false, required: true };
  }

  return null;
}

function readStrategyName(value: unknown): string | null {
  if (isRecord(value)) {
    return text(pick(value, "id", "key", "name", "label"));
  }

  return text(value);
}

function readStatus(raw: unknown): PackStatus | null {
  if (!isRecord(raw)) {
    return null;
  }

  const pkg = isRecord(raw.package) ? raw.package : raw;
  const source = isRecord(raw.status) ? raw.status : raw;
  const version = text(pick(pkg, "version", "packVersion")) ?? text(pick(raw, "version"));
  const stateText = typeof source.state === "string" ? source.state.toLowerCase() : null;

  const running =
    flag(pick(source, "running", "active", "started", "isRunning")) ??
    (stateText === null
      ? flag(pick(pkg, "running", "active"))
      : stateText === "running" || stateText === "started"
        ? true
        : stateText === "stopped" || stateText === "idle"
          ? false
          : null) ??
    false;

  const installed =
    flag(pick(pkg, "installed", "isInstalled", "present", "hasPackage")) ??
    (version !== null || text(pick(pkg, "path", "root", "folder")) !== null);

  const errorNode = pick(source, "lastError", "last_error", "error");
  const lastError =
    typeof errorNode === "string"
      ? text(errorNode)
      : isRecord(errorNode)
        ? text(pick(errorNode, "message", "text", "reason"))
        : null;

  return {
    installed,
    version,
    path: text(pick(pkg, "path", "root", "folder")),
    strategy: readStrategyName(pick(source, "strategy", "activeStrategy", "current")),
    running,
    lastError,
    admin: readAdmin(raw),
  };
}

function readStrategies(raw: unknown): Strategy[] {
  const list = Array.isArray(raw)
    ? raw
    : isRecord(raw) && Array.isArray(raw.strategies)
      ? raw.strategies
      : isRecord(raw) && Array.isArray(raw.items)
        ? raw.items
        : [];

  const strategies: Strategy[] = [];
  const seen = new Set<string>();

  for (const entry of list) {
    const id = entryId(entry);
    if (!id || seen.has(id)) {
      continue;
    }

    seen.add(id);
    const record = isRecord(entry) ? entry : {};
    strategies.push({
      id,
      name: text(pick(record, "name", "label", "title")) ?? id,
      description: text(pick(record, "description", "desc", "hint", "summary")),
    });
  }

  return strategies;
}

function readSelected(raw: unknown): string | null {
  if (!isRecord(raw)) {
    return null;
  }

  return readStrategyName(pick(raw, "selected", "active", "current", "strategy"));
}

function checkVerdict(entry: RecordValue): boolean {
  const explicit = flag(pick(entry, "ok", "passed", "pass", "success", "healthy"));
  if (explicit !== null) {
    return explicit;
  }

  const status = text(pick(entry, "status", "state", "result"))?.toLowerCase() ?? "";
  if (["ok", "pass", "passed", "success", "healthy", "good"].includes(status)) {
    return true;
  }

  if (["fail", "failed", "error", "bad", "missing"].includes(status)) {
    return false;
  }

  return false;
}

function readChecks(raw: unknown): Check[] {
  const list = Array.isArray(raw)
    ? raw
    : isRecord(raw) && Array.isArray(raw.checks)
      ? raw.checks
      : isRecord(raw) && Array.isArray(raw.results)
        ? raw.results
        : [];

  const checks: Check[] = [];
  for (const entry of list) {
    if (typeof entry === "string") {
      checks.push({ id: entry, name: entry, hint: null, ok: true });
      continue;
    }

    if (!isRecord(entry)) {
      continue;
    }

    const name = text(pick(entry, "name", "label", "title", "id", "check"));
    if (!name) {
      continue;
    }

    checks.push({
      id: text(entry.id) ?? name,
      name,
      hint: text(pick(entry, "hint", "detail", "details", "message", "description")),
      ok: checkVerdict(entry),
    });
  }

  return checks;
}

export function createZapretScreen(ctx: AppContext): Screen {
  const t = (key: MessageKey, params?: Record<string, string | number>): string =>
    ctx.i18n.t(key, params);
  const l = (key: keyof typeof copy.ru): string => copy[ctx.i18n.language][key];

  const root = document.createElement("section");
  root.className = "screen zapret-screen";
  root.dataset.screen = "zapret";

  let loading = true;
  let unavailable = false;
  let failed = false;
  let status: PackStatus | null = null;
  let strategies: Strategy[] = [];
  let selected: string | null = null;
  let busy: Busy = null;
  let importPath = "";
  let importError = false;
  let manualOpen = false;
  let detectedPath: string | null = null;
  let detectMiss = false;
  let testChecks: Check[] | null = null;
  let testRunning = false;
  let testUnavailable = false;
  let testFailed = false;

  const strategyName = (id: string | null): string => {
    if (!id) {
      return t("common.na");
    }

    return strategies.find((entry) => entry.id === id)?.name ?? id;
  };

  const applySelected = (id: string | null): void => {
    selected = id;
    const list = root.querySelector<HTMLElement>("#zapret-strategies");
    if (!list) {
      return;
    }

    for (const row of list.querySelectorAll<HTMLElement>(".strategy")) {
      const active = row.dataset.strategy === id;
      row.setAttribute("aria-checked", String(active));
      row.classList.toggle("strategy--selected", active);
      row.tabIndex = active ? 0 : -1;

      const marks = row.querySelector<HTMLElement>(".strategy__marks");
      if (!marks) {
        continue;
      }

      marks.replaceChildren();
      if (status?.running === true && status.strategy === row.dataset.strategy) {
        marks.append(pill(t("zapret.strategy.current"), "accent"));
      } else if (active) {
        marks.append(pill(t("zapret.strategy.selected"), "neutral"));
      }
    }
  };

  const load = async (quiet = false): Promise<void> => {
    if (!quiet) {
      loading = true;
      unavailable = false;
      failed = false;
      render();
    }

    const [stateResult, listResult] = await Promise.all([
      ctx.host.call<unknown>("zapret.status"),
      ctx.host.call<unknown>("zapret.listStrategies"),
    ]);

    loading = false;

    if (!stateResult.ok) {
      status = null;
      strategies = [];
      unavailable = stateResult.reason === "unavailable";
      failed = !unavailable;
      render();
      return;
    }

    unavailable = false;
    failed = false;
    const nextStatus = readStatus(stateResult.data) ?? {
      installed: false,
      version: null,
      path: null,
      strategy: null,
      running: false,
      lastError: null,
      admin: null,
    };
    status = nextStatus;

    strategies = listResult.ok ? readStrategies(listResult.data) : [];
    const selectedByHost = listResult.ok ? readSelected(listResult.data) : null;
    selected =
      selectedByHost ??
      (nextStatus.strategy && strategies.some((entry) => entry.id === nextStatus.strategy)
        ? nextStatus.strategy
        : null) ??
      strategies[0]?.id ??
      nextStatus.strategy;

    render();
  };

  const start = async (): Promise<void> => {
    if (busy !== null || !status?.installed) {
      return;
    }

    const id = selected ?? strategies[0]?.id ?? null;
    if (!id) {
      return;
    }

    busy = "start";
    render();

    const result = await ctx.host.call<unknown>("zapret.start", { strategy: id });
    busy = null;

    if (!result.ok) {
      showToast(
        t(result.reason === "unavailable" ? "toast.unavailable" : "zapret.startFailed"),
        "error",
      );
      await load(true);
      return;
    }

    showToast(t("zapret.started"), "success");
    await load(true);
  };

  const stop = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "stop";
    render();

    const result = await ctx.host.call<unknown>("zapret.stop");
    busy = null;

    if (!result.ok) {
      showToast(
        t(result.reason === "unavailable" ? "toast.unavailable" : "zapret.stopFailed"),
        "error",
      );
      await load(true);
      return;
    }

    showToast(t("zapret.stoppedToast"), "success");
    await load(true);
  };

  const runImport = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    const path = importPath.trim();
    if (path.length === 0) {
      importError = true;
      render();
      const input = root.querySelector<HTMLInputElement>("#zapret-import-path");
      input?.focus();
      return;
    }

    importError = false;
    busy = "import";
    render();

    const result = await ctx.host.call<unknown>("zapret.import", { path });
    busy = null;

    if (!result.ok) {
      showToast(
        t(result.reason === "unavailable" ? "toast.unavailable" : "zapret.import.failed"),
        "error",
      );
      render();
      return;
    }

    const next = readStatus(result.data);
    if (next) {
      status = next;
    }

    detectedPath = path;
    detectMiss = false;

    showToast(t("zapret.import.done"), "success");
    await load(true);
  };

  /** `zapret.pickPack`: the host opens a folder dialog and imports what was chosen. */
  const pickPack = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "pick";
    detectMiss = false;
    render();

    const result = await ctx.host.call<unknown>("zapret.pickPack");
    busy = null;

    if (!result.ok) {
      showToast(
        t(result.reason === "unavailable" ? "toast.unavailable" : "zapret.import.failed"),
        "error",
      );
      render();
      return;
    }

    const payload = isRecord(result.data) ? result.data : {};
    if (flag(payload.cancelled) === true) {
      render();
      return;
    }

    const path = text(payload.path);
    if (path) {
      detectedPath = path;
      importPath = path;
    }

    const next = readStatus(payload.state);
    if (next) {
      status = next;
    }

    showToast(t("zapret.import.done"), "success");
    await load(true);
  };

  /** `zapret.detectPack`: the host searches the known folders and imports the first pack. */
  const detectPack = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "detect";
    detectMiss = false;
    render();

    const result = await ctx.host.call<unknown>("zapret.detectPack");
    busy = null;

    if (!result.ok) {
      showToast(
        t(result.reason === "unavailable" ? "toast.unavailable" : "zapret.import.failed"),
        "error",
      );
      render();
      return;
    }

    const payload = isRecord(result.data) ? result.data : {};
    if (flag(payload.found) !== true) {
      detectMiss = true;
      render();
      return;
    }

    const path = text(payload.path);
    if (path) {
      detectedPath = path;
      importPath = path;
    }

    const next = readStatus(payload.state);
    if (next) {
      status = next;
    }

    showToast(t("zapret.import.done"), "success");
    await load(true);
  };

  const runSelfTest = async (): Promise<void> => {
    if (testRunning) {
      return;
    }

    testRunning = true;
    testUnavailable = false;
    testFailed = false;
    render();

    const result = await ctx.host.call<unknown>("zapret.selfTest");
    testRunning = false;

    if (!result.ok) {
      testChecks = null;
      testUnavailable = result.reason === "unavailable";
      testFailed = !testUnavailable;
      if (testFailed) {
        showToast(t("zapret.test.failedRun"), "error");
      }

      render();
      return;
    }

    testChecks = readChecks(result.data);
    render();

    if (testChecks.length === 0) {
      return;
    }

    const failedChecks = testChecks.filter((check) => !check.ok).length;
    showToast(
      t(failedChecks > 0 ? "zapret.test.issues" : "zapret.test.pass"),
      failedChecks > 0 ? "error" : "success",
    );
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", t("zapret.title")),
      el("p", "screen__subtitle", t("zapret.subtitle")),
    );

    const aside = el("div", "screen__aside");
    if (status) {
      aside.append(
        pill(
          t(status.installed ? "zapret.status.installed" : "zapret.status.missing"),
          status.installed ? "on" : "neutral",
        ),
      );

      if (status.installed) {
        aside.append(
          pill(t(status.running ? "zapret.running" : "zapret.stopped"), status.running ? "accent" : "neutral"),
        );
      }
    }

    head.append(copy, aside);
    return head;
  };

  const renderImport = (): HTMLElement => {
    const wrap = el("div", "zapret__import");

    const actions = el("div", "zapret__importActions");

    const pick = button({
      label: busy === "pick" ? l("picking") : l("pick"),
      variant: "primary",
      icon: "folder",
      onClick: () => void pickPack(),
    });
    pick.id = "zapret-pick-pack";
    pick.disabled = busy !== null;
    if (busy === "pick") {
      setBusy(pick, true, l("picking"));
    }

    const detect = button({
      label: busy === "detect" ? l("detecting") : l("detect"),
      variant: "secondary",
      icon: "launcher",
      onClick: () => void detectPack(),
    });
    detect.id = "zapret-detect-pack";
    detect.disabled = busy !== null;
    if (busy === "detect") {
      setBusy(detect, true, l("detecting"));
    }

    actions.append(pick, detect);
    wrap.append(actions);

    if (detectedPath) {
      const found = el("div", "zapret__detected");
      found.append(
        el("span", "zapret__detectedLabel", l("detected")),
        el("code", "zapret__detectedPath", detectedPath),
      );
      wrap.append(found);
    }

    if (detectMiss) {
      wrap.append(el("p", "zapret__detectMiss", l("detectMiss")));
    }

    const toggle = button({
      label: manualOpen ? l("manualHide") : l("manual"),
      variant: "ghost",
      size: "sm",
      icon: manualOpen ? "chevronUp" : "chevronDown",
      onClick: () => {
        if (busy !== null) {
          return;
        }

        manualOpen = !manualOpen;
        render();
        if (manualOpen) {
          root.querySelector<HTMLInputElement>("#zapret-import-path")?.focus();
        }
      },
    });
    toggle.id = "zapret-manual-toggle";
    toggle.setAttribute("aria-expanded", String(manualOpen));
    toggle.disabled = busy !== null;
    wrap.append(toggle);

    if (!manualOpen) {
      return wrap;
    }

    const manual = el("div", "zapret__manual");

    const label = el("label", "field__label", t("zapret.import.label"));
    label.htmlFor = "zapret-import-path";

    const input = document.createElement("input");
    input.type = "text";
    input.id = "zapret-import-path";
    input.className = "input input--mono";
    input.spellcheck = false;
    input.autocomplete = "off";
    input.placeholder = t("zapret.import.placeholder");
    input.value = importPath;
    input.disabled = busy !== null;
    if (importError) {
      input.setAttribute("aria-invalid", "true");
    }

    input.addEventListener("input", () => {
      importPath = input.value;
      if (importError && input.value.trim() !== "") {
        importError = false;
        input.removeAttribute("aria-invalid");
        const error = manual.querySelector<HTMLElement>(".field__error");
        if (error) {
          error.hidden = true;
        }
      }
    });

    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void runImport();
      }
    });

    const action = button({
      label: busy === "import" ? t("zapret.import.busy") : t("zapret.import.action"),
      variant: "secondary",
      icon: "download",
      onClick: () => void runImport(),
    });
    action.id = "zapret-import";
    action.disabled = busy !== null;
    if (busy === "import") {
      setBusy(action, true, t("zapret.import.busy"));
    }

    const row = el("div", "zapret__importRow");
    row.append(input, action);

    const error = el("span", "field__error", t("zapret.import.empty"));
    error.hidden = !importError;

    manual.append(label, row, error);
    wrap.append(manual);
    return wrap;
  };

  const renderAdmin = (admin: AdminInfo): HTMLElement => {
    const note = el("div", `callout${admin.elevated ? " callout--ok" : " callout--warn"}`);
    const glyph = el("span", "callout__icon");
    glyph.innerHTML = icon("shield", 17);

    const copy = el("div", "callout__copy");
    copy.append(
      el("span", "callout__title", admin.elevated ? t("zapret.admin.elevated") : t("zapret.admin.title")),
    );

    if (!admin.elevated) {
      if (admin.path.length > 0) {
        copy.append(el("span", "callout__text", t("zapret.admin.text", { path: admin.path })));
      }

      copy.append(el("span", "callout__hint", t("zapret.admin.hint")));
    }

    note.append(glyph, copy);
    return note;
  };

  const renderState = (): HTMLElement => {
    const wrap = el("div", "zapret__state");
    if (!status) {
      return wrap;
    }

    if (!status.installed) {
      wrap.append(
        emptyState({
          icon: "network",
          title: t("zapret.empty.title"),
          text: t("zapret.empty.text"),
        }),
      );
      return wrap;
    }

    const grid = el("div", "meta-grid meta-grid--wide");
    grid.append(
      metaRow(t("zapret.status.version"), status.version ?? t("common.na"), true),
      metaRow(t("zapret.status.path"), status.path ?? t("common.na"), true),
      metaRow(t("zapret.status.strategy"), strategyName(status.strategy), false),
      metaRow(t("zapret.status.state"), t(status.running ? "zapret.running" : "zapret.stopped"), false),
    );

    const errorRow = el("div", "meta-row meta-row--wide");
    errorRow.append(
      el("span", "meta-row__label", t("zapret.status.lastError")),
      el("span", "meta-row__value", status.lastError ?? t("zapret.status.noError")),
    );

    wrap.append(grid, errorRow);
    return wrap;
  };

  const renderStatusCard = (): HTMLElement => {
    const card = panel({
      title: t("zapret.status.title"),
      icon: "network",
      className: "zapret__status",
    });

    card.body.append(renderState());
    return card.el;
  };

  const renderImportCard = (): HTMLElement => {
    const card = panel({
      title: t("zapret.import.title"),
      hint: t("zapret.import.hint"),
      icon: "folder",
      className: "zapret__importCard",
    });

    card.body.append(renderImport());

    if (status?.admin) {
      card.body.append(renderAdmin(status.admin));
    }

    return card.el;
  };

  const renderStrategies = (): HTMLElement => {
    const startButton = button({
      label: busy === "start" ? t("zapret.starting") : t("zapret.start"),
      variant: "primary",
      size: "sm",
      icon: "play",
      onClick: () => void start(),
    });
    startButton.id = "zapret-start";
    startButton.disabled = busy !== null || status?.installed !== true || !selected || status.running === true;
    if (busy === "start") {
      setBusy(startButton, true, t("zapret.starting"));
    }

    const stopButton = button({
      label: busy === "stop" ? t("zapret.stopping") : t("zapret.stop"),
      variant: "secondary",
      size: "sm",
      icon: "pause",
      onClick: () => void stop(),
    });
    stopButton.id = "zapret-stop";
    stopButton.disabled = busy !== null || status?.running !== true;
    if (busy === "stop") {
      setBusy(stopButton, true, t("zapret.stopping"));
    }

    const card = panel({
      title: t("zapret.strategies.title"),
      hint: t("zapret.strategies.hint"),
      icon: "network",
      actions: [startButton, stopButton],
      className: "zapret__strategies",
    });

    const list = el("div", "strategy-list");
    list.id = "zapret-strategies";
    list.setAttribute("role", "radiogroup");
    list.setAttribute("aria-label", t("zapret.strategies.title"));

    if (strategies.length === 0) {
      card.body.append(
        el(
          "p",
          "panel__note",
          t(status?.installed ? "zapret.strategies.empty" : "zapret.strategies.needPack"),
        ),
      );
    } else {
      for (const strategy of strategies) {
        const active = strategy.id === selected;
        const current = status?.running === true && status.strategy === strategy.id;
        const row = el("button", `strategy${active ? " strategy--selected" : ""}`);
        row.type = "button";
        row.dataset.strategy = strategy.id;
        row.setAttribute("role", "radio");
        row.setAttribute("aria-checked", String(active));
        row.tabIndex = active ? 0 : -1;
        row.disabled = busy !== null;

        const dot = el("span", "strategy__dot");
        const copy = el("span", "strategy__copy");
        copy.append(el("span", "strategy__name", strategy.name));
        if (strategy.description) {
          copy.append(el("span", "strategy__desc", strategy.description));
        }

        const marks = el("span", "strategy__marks");
        if (current) {
          marks.append(pill(t("zapret.strategy.current"), "accent"));
        } else if (active) {
          marks.append(pill(t("zapret.strategy.selected"), "neutral"));
        }

        row.append(dot, copy, marks);
        row.addEventListener("click", () => applySelected(strategy.id));
        list.append(row);
      }
    }

    list.addEventListener("keydown", (event) => {
      if (busy !== null || strategies.length === 0) {
        return;
      }

      const rows = Array.from(list.querySelectorAll<HTMLButtonElement>(".strategy"));
      const index = rows.findIndex((row) => row.dataset.strategy === selected);

      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        void start();
        return;
      }

      let nextIndex: number | null = null;
      if (event.key === "ArrowDown") {
        nextIndex = index < 0 ? 0 : (index + 1) % rows.length;
      } else if (event.key === "ArrowUp") {
        nextIndex = index <= 0 ? rows.length - 1 : index - 1;
      } else if (event.key === "Home") {
        nextIndex = 0;
      } else if (event.key === "End") {
        nextIndex = rows.length - 1;
      }

      if (nextIndex === null) {
        return;
      }

      const row = rows[nextIndex];
      if (!row) {
        return;
      }

      event.preventDefault();
      applySelected(row.dataset.strategy ?? null);
      row.focus();
      row.scrollIntoView({ block: "nearest" });
    });

    card.body.append(list);
    return card.el;
  };

  const renderSelfTest = (): HTMLElement => {
    const run = button({
      label: testRunning ? t("zapret.test.running") : t("zapret.test.run"),
      variant: "secondary",
      size: "sm",
      icon: "shield",
      onClick: () => void runSelfTest(),
    });
    run.id = "zapret-selftest";
    run.disabled = testRunning;
    if (testRunning) {
      setBusy(run, true, t("zapret.test.running"));
    }

    const card = panel({
      title: t("zapret.test.title"),
      hint: t("zapret.test.hint"),
      icon: "shield",
      actions: [run],
      className: "zapret__test",
    });

    if (testRunning) {
      const rows = el("div", "check-list check-list--loading");
      for (let index = 0; index < 3; index += 1) {
        rows.append(skeleton(34, index === 2 ? "skeleton--short" : ""));
      }

      card.body.append(rows);
      return card.el;
    }

    if (testUnavailable) {
      card.body.append(el("p", "state-note", t("zapret.test.unavailable")));
      return card.el;
    }

    if (testFailed) {
      card.body.append(el("p", "state-note", t("zapret.test.failedRun")));
      return card.el;
    }

    if (!testChecks || testChecks.length === 0) {
      card.body.append(el("p", "panel__note", t("zapret.test.empty")));
      return card.el;
    }

    const failedChecks = testChecks.filter((check) => !check.ok).length;
    card.body.append(
      el(
        "span",
        `zapret__summary${failedChecks > 0 ? " zapret__summary--bad" : ""}`,
        t("zapret.test.summary", { passed: testChecks.length - failedChecks, total: testChecks.length }),
      ),
    );

    const list = el("ul", "check-list");
    for (const check of testChecks) {
      const item = el("li", `check${check.ok ? " check--ok" : " check--bad"}`);
      const dot = el("span", "check__dot");
      const copy = el("span", "check__copy");
      copy.append(el("span", "check__name", check.name));
      if (check.hint) {
        copy.append(el("span", "check__hint", check.hint));
      }

      item.append(dot, copy, el("span", "check__verdict", t(check.ok ? "zapret.test.ok" : "zapret.test.bad")));
      list.append(item);
    }

    card.body.append(list);
    return card.el;
  };

  const renderMessage = (title: string, body: string): HTMLElement => {
    const card = panel({});
    const note = el("div", "state-note");
    note.append(el("strong", "state-note__title", title), el("span", "state-note__text", body));
    const retry = button({
      label: t("common.retry"),
      variant: "secondary",
      size: "sm",
      icon: "refresh",
      onClick: () => void load(),
    });
    card.body.append(note, retry);
    return card.el;
  };

  const render = (): void => {
    if (loading) {
      const grid = el("div", "zapret__grid");
      grid.append(skeletonPanel(4), skeletonPanel(4));
      root.replaceChildren(renderHeader(), skeletonPanel(3), grid);
      return;
    }

    if (unavailable) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(t("zapret.unavailable.title"), t("zapret.unavailable.text")),
      );
      return;
    }

    if (failed || !status) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(t("zapret.failed.title"), t("zapret.failed.text")),
      );
      return;
    }

    const grid = el("div", "zapret__grid");
    const side = el("div", "zapret__side");
    side.append(renderImportCard(), renderSelfTest());
    grid.append(renderStrategies(), side);
    root.replaceChildren(renderHeader(), renderStatusCard(), grid);
  };

  const activate = async (): Promise<void> => {
    await load();
  };

  return { el: root, activate, render };
}
