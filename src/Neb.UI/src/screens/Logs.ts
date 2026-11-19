import type { AppContext, Screen } from "../context";
import { button, el, emptyState, formatTime, panel, skeleton, switchControl } from "../components/controls";
import { showToast } from "../components/toast";

const TailLimit = 200;
const LevelPattern = /\b(ERROR|WARN|WARNING|INFO|DEBUG|TRACE)\b/;

function readLines(raw: unknown): string[] {
  if (typeof raw === "string") {
    return raw.split(/\r?\n/).filter((line) => line.length > 0);
  }

  if (Array.isArray(raw)) {
    return raw
      .map((entry) => (typeof entry === "string" ? entry : JSON.stringify(entry)))
      .filter((line) => line.length > 0);
  }

  if (typeof raw === "object" && raw !== null) {
    const record = raw as Record<string, unknown>;
    const lines = record.lines ?? record.tail ?? record.entries ?? record.content;
    if (lines !== undefined && lines !== raw) {
      return readLines(lines);
    }
  }

  return [];
}

function levelOf(line: string): "error" | "warn" | "info" | "dim" {
  const match = LevelPattern.exec(line);
  const level = match?.[1]?.toUpperCase() ?? "";
  if (level === "ERROR") {
    return "error";
  }

  if (level === "WARN" || level === "WARNING") {
    return "warn";
  }

  if (level === "INFO") {
    return "info";
  }

  return "dim";
}

export function createLogsScreen(ctx: AppContext): Screen {
  const root = document.createElement("section");
  root.className = "screen logs-screen";
  root.dataset.screen = "logs";

  let lines: string[] | null = null;
  let loading = true;
  let failed = false;
  let auto = false;
  let updatedAt: string | null = null;
  let logsPath = "%LocalAppData%\\ProjectNeb\\logs";
  let timer = 0;
  let inFlight = false;

  const stopTimer = (): void => {
    window.clearInterval(timer);
    timer = 0;
  };

  const startTimer = (): void => {
    stopTimer();
    timer = window.setInterval(() => {
      void load(true);
    }, 3000);
  };

  const load = async (quiet = false): Promise<void> => {
    if (inFlight) {
      return;
    }

    inFlight = true;
    if (!quiet) {
      loading = true;
      failed = false;
      render();
    }

    const result = await ctx.host.call<unknown>("logs.tail", {
      lines: TailLimit,
      limit: TailLimit,
    });
    inFlight = false;
    loading = false;

    if (result.ok) {
      if (
        typeof result.data === "object" &&
        result.data !== null &&
        typeof (result.data as { path?: unknown }).path === "string"
      ) {
        logsPath = (result.data as { path: string }).path;
      }

      lines = readLines(result.data).slice(-TailLimit);
      failed = false;
      updatedAt = new Date().toISOString();
    } else if (!quiet || lines === null) {
      failed = true;
      lines = null;
      if (!quiet) {
        ctx.notifyFailure(result);
      }
    }

    render();
  };

  const openFolder = (): void => {
    void (async () => {
      const result = await ctx.host.call<unknown>("app.openLogsFolder");
      if (!result.ok) {
        ctx.notifyFailure(result);
      }
    })();
  };

  const toggleAuto = (next: boolean): void => {
    auto = next;
    if (auto) {
      startTimer();
      showToast(ctx.i18n.t("logs.auto"), "info");
    } else {
      stopTimer();
    }

    render();
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", ctx.i18n.t("logs.title")),
      el("p", "screen__subtitle", ctx.i18n.t("logs.subtitle")),
    );

    const aside = el("div", "screen__aside");
    if (lines) {
      aside.append(el("span", "pill pill--neutral", ctx.i18n.t("logs.lines", { count: lines.length })));
    }

    head.append(copy, aside);
    return head;
  };

  const renderToolbar = (): HTMLElement => {
    const bar = el("div", "logs__toolbar");
    const left = el("div", "logs__toolbarGroup");
    const refresh = button({
      label: ctx.i18n.t("logs.refresh"),
      variant: "secondary",
      size: "sm",
      icon: "refresh",
      onClick: () => void load(),
    });
    refresh.id = "logs-refresh";

    const folder = button({
      label: ctx.i18n.t("logs.openFolder"),
      variant: "ghost",
      size: "sm",
      icon: "folder",
      onClick: openFolder,
    });
    folder.id = "logs-folder";

    left.append(refresh, folder);

    const right = el("div", "logs__toolbarGroup");
    const autoLabel = el("span", "logs__autoLabel");
    autoLabel.append(
      el("span", "field__label", ctx.i18n.t("logs.auto")),
      el("span", "logs__autoHint", ctx.i18n.t("logs.autoHint")),
    );
    right.append(
      autoLabel,
      switchControl({
        checked: auto,
        label: ctx.i18n.t("logs.auto"),
        onToggle: toggleAuto,
      }),
    );

    bar.append(left, right);
    return bar;
  };

  const renderBody = (): HTMLElement => {
    if (loading) {
      const view = el("div", "logs__view logs__view--loading");
      for (let index = 0; index < 12; index += 1) {
        view.append(skeleton(11, index % 3 === 2 ? "skeleton--short" : ""));
      }

      return view;
    }

    if (failed) {
      return emptyState({
        icon: "terminal",
        title: ctx.i18n.t("logs.errorTitle"),
        text: ctx.i18n.t("logs.errorText"),
        action: button({
          label: ctx.i18n.t("common.retry"),
          variant: "primary",
          icon: "refresh",
          onClick: () => void load(),
        }),
      });
    }

    if (!lines || lines.length === 0) {
      return emptyState({
        icon: "terminal",
        title: ctx.i18n.t("logs.emptyTitle"),
        text: ctx.i18n.t("logs.emptyText"),
        action: button({
          label: ctx.i18n.t("logs.refresh"),
          variant: "primary",
          icon: "refresh",
          onClick: () => void load(),
        }),
      });
    }

    const view = el("div", "logs__view");
    view.id = "logs-view";
    view.setAttribute("role", "log");
    view.setAttribute("aria-label", ctx.i18n.t("logs.title"));
    view.tabIndex = 0;

    for (const line of lines) {
      view.append(el("div", `log-line log-line--${levelOf(line)}`, line));
    }

    return view;
  };

  const renderMeta = (): HTMLElement => {
    const meta = el("div", "logs__meta");
    meta.append(
      el("span", "logs__path", logsPath),
    );

    if (updatedAt) {
      meta.append(
        el("span", "logs__updated", ctx.i18n.t("logs.updated", { time: formatTime(updatedAt, ctx.i18n.language) })),
      );
    }

    return meta;
  };

  const render = (): void => {
    const card = panel({
      className: "logs-screen__panel",
    });

    card.body.append(renderToolbar(), renderMeta(), renderBody());
    root.replaceChildren(renderHeader(), card.el);

    const view = root.querySelector<HTMLElement>("#logs-view");
    if (view) {
      view.scrollTop = view.scrollHeight;
    }
  };

  const activate = async (): Promise<void> => {
    render();
    await load();
    if (auto && timer === 0) {
      startTimer();
    }
  };

  return {
    el: root,
    activate,
    deactivate() {
      stopTimer();
    },
    render,
  };
}
