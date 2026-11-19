import type { AppContext, Screen } from "../context";
import type { MessageKey } from "../i18n";
import type { ProcessMetrics, ShellConfig } from "../state";
import { formatMemory } from "../state";
import { el, panel, switchControl } from "../components/controls";
import { icon, type IconName } from "../components/icons";
import { confirmDialog } from "../components/Modal";
import { showToast } from "../components/toast";
import { watchFocus } from "../components/focus";

type ComponentId = "bar" | "dock" | "launcher" | "wallpaper";

const components: ReadonlyArray<{
  id: ComponentId;
  field: keyof ShellConfig;
  icon: IconName;
  title: MessageKey;
  desc: MessageKey;
}> = [
  { id: "bar", field: "barEnabled", icon: "bar", title: "overview.components.bar", desc: "overview.components.barDesc" },
  { id: "dock", field: "dockEnabled", icon: "dock", title: "overview.components.dock", desc: "overview.components.dockDesc" },
  {
    id: "launcher",
    field: "launcherEnabled",
    icon: "launcher",
    title: "overview.components.launcher",
    desc: "overview.components.launcherDesc",
  },
  {
    id: "wallpaper",
    field: "wallpaperEnabled",
    icon: "wallpapers",
    title: "overview.components.wallpaper",
    desc: "overview.components.wallpaperDesc",
  },
];

const metricMeta: ReadonlyArray<{
  id: "bar" | "wallpaper" | "supervisor";
  icon: IconName;
  label: MessageKey;
  hint: MessageKey;
}> = [
  { id: "bar", icon: "bar", label: "overview.metrics.bar", hint: "overview.metrics.barHint" },
  { id: "wallpaper", icon: "wallpapers", label: "overview.metrics.wallpaper", hint: "overview.metrics.wallpaperHint" },
  { id: "supervisor", icon: "shield", label: "overview.metrics.supervisor", hint: "overview.metrics.supervisorHint" },
];

export interface OverviewScreen extends Screen {
  /** Updates the three live numbers without rebuilding the screen. */
  setMetrics(): void;
}

export function createOverview(ctx: AppContext): OverviewScreen {
  const root = document.createElement("section");
  root.className = "screen overview";
  root.dataset.screen = "overview";

  const t = (key: MessageKey, params?: Record<string, string | number>): string =>
    ctx.i18n.t(key, params);

  const busy = new Set<string>();
  let lastCheck: string | null = null;
  let activated = false;
  const focus = watchFocus(root);

  /**
   * «Работает / Частично / Остановлено». The processes' own `running` flags win; the component
   * configuration is only the fallback for a host that does not report them yet.
   */
  const status = (): { label: string; tone: "on" | "warn" | "danger" | "neutral"; hint: string } => {
    const metrics = ctx.metrics();
    const flags = [metrics?.shell.running, metrics?.wallpaper.running, metrics?.supervisor.running].filter(
      (flag): flag is boolean => typeof flag === "boolean",
    );

    if (flags.length > 0) {
      const live = flags.filter(Boolean).length;
      if (live === flags.length) {
        return {
          label: ctx.i18n.t("status.running"),
          tone: "on",
          hint: ctx.i18n.t("overview.runningHint"),
        };
      }

      if (live === 0) {
        return {
          label: ctx.i18n.t("status.stopped"),
          tone: "danger",
          hint: ctx.i18n.t("overview.stoppedHint"),
        };
      }

      return {
        label: ctx.i18n.t("status.partial"),
        tone: "warn",
        hint: ctx.i18n.t("overview.partialHint"),
      };
    }

    const shell = ctx.shell();
    if (!shell) {
      return {
        label: ctx.i18n.t("status.unknown"),
        tone: "neutral",
        hint: ctx.i18n.t("overview.metrics.empty"),
      };
    }

    const config = ctx.config().shell;
    if (!shell.running) {
      return {
        label: ctx.i18n.t("status.stopped"),
        tone: "danger",
        hint: ctx.i18n.t("overview.stoppedHint"),
      };
    }

    const enabled = components.filter((component) => config[component.field] === true).length;
    if (enabled === components.length) {
      return {
        label: ctx.i18n.t("status.running"),
        tone: "on",
        hint: ctx.i18n.t("overview.runningHint"),
      };
    }

    if (enabled === 0) {
      return {
        label: ctx.i18n.t("status.stopped"),
        tone: "danger",
        hint: ctx.i18n.t("overview.stoppedHint"),
      };
    }

    return {
      label: ctx.i18n.t("status.partial"),
      tone: "warn",
      hint: ctx.i18n.t("overview.partialHint"),
    };
  };

  const processFor = (id: "bar" | "wallpaper" | "supervisor"): ProcessMetrics | null => {
    const metrics = ctx.metrics();
    if (!metrics) {
      return null;
    }

    return id === "bar" ? metrics.shell : metrics[id];
  };

  const metricHint = (meta: (typeof metricMeta)[number], process: ProcessMetrics | null): string => {
    const hint = ctx.i18n.t(meta.hint);
    if (!process) {
      return `${hint} · ${ctx.i18n.t("overview.metrics.empty")}`;
    }

    const parts = [hint];
    if (process.pid !== null) {
      parts.push(ctx.i18n.t("overview.metrics.pid", { pid: process.pid }));
    }

    if (process.running === false) {
      parts.push(ctx.i18n.t("status.stopped"));
    }

    return parts.join(" · ");
  };

  const updateStatus = (): void => {
    const node = root.querySelector<HTMLElement>("#overview-status");
    if (!node) {
      return;
    }

    const state = status();
    node.className = `status status--${state.tone}`;
    node.title = state.hint;
    const label = node.querySelector(".status__label");
    if (label) {
      label.textContent = state.label;
    }
  };

  const updateMetrics = (): void => {
    const unit = ctx.i18n.t("common.megabytes");

    for (const meta of metricMeta) {
      const card = root.querySelector<HTMLElement>(`[data-metric="${meta.id}"]`);
      if (!card) {
        continue;
      }

      const process = processFor(meta.id);
      const value = process?.mb ?? null;
      const valueNode = card.querySelector(".metric__value");
      if (valueNode) {
        valueNode.textContent = value === null ? "—" : formatMemory(value, ctx.i18n.language, unit);
      }

      card.classList.toggle("metric--empty", value === null);
      const hintNode = card.querySelector(".metric__hint");
      if (hintNode) {
        hintNode.textContent = metricHint(meta, process);
      }
    }

    updateStatus();
  };

  const withBusy = async (id: string, action: () => Promise<void>): Promise<void> => {
    if (busy.has(id)) {
      return;
    }

    busy.add(id);
    render();
    await action();
    busy.delete(id);
    render();
    root.querySelector<HTMLElement>(`[data-toggle="${id}"]`)?.focus();
  };

  const toggleComponent = (component: (typeof components)[number], next: boolean): void => {
    void withBusy(component.id, async () => {
      if (component.id === "wallpaper") {
        const result = await ctx.host.call<unknown>("wallpaper.setSettings", { enabled: next });
        if (result.ok) {
          ctx.setLocalConfig("shell.wallpaperEnabled", next);
          await ctx.refreshShell();
          return;
        }

        ctx.notifyFailure(result);
        return;
      }

      const saved = await ctx.saveConfig(`shell.${component.field}`, next);
      if (!saved) {
        // `saveConfig` already showed the host's own error; a generic second toast would hide it.
        return;
      }

      // The running shell picks the new list up on reload; a host without the method simply keeps
      // its current children and the push of `neb.modules` updates the toggles.
      const reload = await ctx.host.call<unknown>("shell.reload");
      if (!reload.ok && reload.reason === "failed") {
        ctx.notify(ctx.i18n.t("toast.restartFailed"), "error");
      }

      await ctx.refreshShell();
    });
  };

  const checkNow = (node: HTMLButtonElement): void => {
    void (async () => {
      node.classList.add("is-busy");
      node.disabled = true;
      const verdict = await ctx.checkUpdates();
      node.classList.remove("is-busy");
      node.disabled = false;

      if (!verdict) {
        lastCheck = ctx.i18n.t("overview.check.failed");
        render();
        return;
      }

      if (verdict.available) {
        lastCheck = ctx.i18n.t("overview.check.available", { version: verdict.version ?? "" });
        showToast(lastCheck, "success");
      } else {
        lastCheck = ctx.i18n.t("overview.check.none");
        showToast(lastCheck, "success");
      }

      render();
    })();
  };

  const restartShell = (): void => {
    void (async () => {
      const confirmed = await confirmDialog({
        title: ctx.i18n.t("modal.restart.title"),
        text: ctx.i18n.t("modal.restart.text"),
        confirm: ctx.i18n.t("modal.restart.confirm"),
        cancel: ctx.i18n.t("common.cancel"),
      });

      if (!confirmed) {
        return;
      }

      const result = await ctx.host.call<unknown>("app.restartShell");
      if (result.ok) {
        showToast(ctx.i18n.t("toast.restarted"), "success");
        lastCheck = null;
        render();
        return;
      }

      ctx.notifyFailure(result);
    })();
  };

  const quit = (): void => {
    void (async () => {
      const confirmed = await confirmDialog({
        title: ctx.i18n.t("modal.quit.title"),
        text: ctx.i18n.t("modal.quit.text"),
        confirm: ctx.i18n.t("modal.quit.confirm"),
        cancel: ctx.i18n.t("common.cancel"),
        danger: true,
      });

      if (!confirmed) {
        return;
      }

      const result = await ctx.host.call<unknown>("app.quit");
      if (!result.ok) {
        ctx.notifyFailure(result);
      }
    })();
  };

  const openChangelog = (): void => {
    void (async () => {
      const result = await ctx.host.call<unknown>("app.openChangelog");
      if (!result.ok) {
        ctx.notifyFailure(result);
      }
    })();
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");

    const identity = el("div", "screen__identity");
    identity.append(
      el("span", "screen__identityDot"),
      el("span", "screen__identityName", ctx.i18n.t("app.name")),
      el("span", "screen__identityVersion", `v${ctx.version()}`),
    );

    copy.append(
      identity,
      el("h1", "screen__title", ctx.i18n.t("overview.title")),
      el("p", "screen__subtitle", ctx.i18n.t("overview.subtitle")),
    );

    const aside = el("div", "screen__aside");
    const state = status();
    const badge = el("span", `status status--${state.tone}`);
    badge.id = "overview-status";
    badge.append(el("span", "status__dot"), el("span", "status__label", state.label));
    badge.title = state.hint;
    aside.append(badge);

    head.append(copy, aside);
    return head;
  };

  const renderMetrics = (): HTMLElement => {
    const grid = el("div", "metrics");
    const unit = ctx.i18n.t("common.megabytes");

    for (const meta of metricMeta) {
      const process = processFor(meta.id);
      const value = process?.mb ?? null;
      const card = el("article", `metric${value === null ? " metric--empty" : ""}`);
      card.dataset.metric = meta.id;

      const top = el("div", "metric__top");
      const glyph = el("span", "metric__icon");
      glyph.innerHTML = icon(meta.icon, 16);
      top.append(glyph, el("span", "metric__label", ctx.i18n.t(meta.label)));

      card.append(
        top,
        el("div", "metric__value", value === null ? "—" : formatMemory(value, ctx.i18n.language, unit)),
        el("div", "metric__hint", metricHint(meta, process)),
      );
      grid.append(card);
    }

    return grid;
  };

  const renderComponents = (): HTMLElement => {
    const config = ctx.config().shell;
    const shell = ctx.shell();
    const card = panel({
      title: ctx.i18n.t("overview.components.title"),
      hint: ctx.i18n.t("overview.components.hint"),
      icon: "extensions",
      className: "overview__components",
    });

    const list = el("div", "component-list");
    list.setAttribute("role", "list");
    for (const component of components) {
      const row = el("div", "component");
      row.setAttribute("role", "listitem");
      const glyph = el("span", "component__icon");
      glyph.innerHTML = icon(component.icon, 17);

      const copy = el("div", "component__copy");
      copy.append(
        el("span", "component__name", ctx.i18n.t(component.title)),
        el("span", "component__desc", ctx.i18n.t(component.desc)),
      );

      // `neb.modules` is the running truth; the saved config is the fallback before the first push.
      const checked = shell?.modules[component.id] ?? config[component.field] === true;
      const toggle = switchControl({
        checked,
        label: ctx.i18n.t(component.title),
        disabled: busy.has(component.id),
        onToggle: (next) => toggleComponent(component, next),
      });
      toggle.dataset.toggle = component.id;

      row.append(glyph, copy, toggle);
      list.append(row);
    }

    card.body.append(list);

    const version = ctx.i18n.t("overview.version", {
      version: ctx.version(),
      channel: ctx.i18n.t(ctx.channel() === "stable" ? "updates.stable" : "updates.beta"),
    });
    const foot = el("div", "panel__foot");
    foot.append(el("span", "panel__footText", version));
    if (shell?.profile) {
      foot.append(el("span", "panel__footText", ctx.i18n.t("overview.profile", { name: shell.profile })));
    }

    card.el.append(foot);
    return card.el;
  };

  const renderWinStrip = (): HTMLElement => {
    const state = ctx.windows();
    const applied = state ? Object.values(state.applied).filter(Boolean).length : 0;
    const label = !state
      ? t("overview.winmode.unknown")
      : state.enabled
        ? t("overview.winmode.on")
        : t("overview.winmode.off");

    const strip = el("section", "winmode-strip");
    strip.id = "overview-winmode";

    const glyph = el("span", "winmode-strip__icon");
    glyph.innerHTML = icon("taskbar", 17);

    const copy = el("div", "winmode-strip__copy");
    const line = el("span", "winmode-strip__line");
    line.append(
      el("span", "winmode-strip__label", `${t("overview.winmode.label")}:`),
      el("span", `winmode-strip__state${state?.enabled ? " winmode-strip__state--on" : ""}`, label),
    );

    if (state && applied > 0) {
      line.append(
        el("span", "winmode-strip__sep", "·"),
        el("span", "winmode-strip__meta", t("overview.winmode.count", { count: applied })),
      );
    }

    copy.append(line);
    strip.append(glyph, copy);

    const link = el("button", "winmode-strip__link");
    link.type = "button";
    link.title = t("overview.winmode.openHint");
    link.setAttribute("aria-label", t("overview.winmode.openHint"));
    link.append(el("span", "winmode-strip__linkText", t("overview.winmode.open")));
    const chevron = el("span", "winmode-strip__chevron");
    chevron.innerHTML = icon("chevronRight", 15);
    link.append(chevron);
    link.addEventListener("click", () => ctx.navigate("winmode"));

    strip.append(link);
    return strip;
  };

  const renderActions = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("overview.actions.title"),
      icon: "zap",
      className: "overview__actions",
    });

    const grid = el("div", "quick-grid");

    const action = (
      glyph: IconName,
      label: string,
      onClick: (node: HTMLButtonElement) => void,
      options: { id?: string; danger?: boolean } = {},
    ): HTMLButtonElement => {
      const node = el("button", `quick-action${options.danger ? " quick-action--danger" : ""}`);
      node.type = "button";
      if (options.id) {
        node.id = options.id;
      }

      const box = el("span", "quick-action__icon");
      box.innerHTML = icon(glyph, 17);
      node.append(box, el("span", "quick-action__label", label));
      node.addEventListener("click", () => onClick(node));
      return node;
    };

    grid.append(
      action("download", ctx.i18n.t("overview.actions.check"), (node) => checkNow(node), { id: "overview-check" }),
      action("refresh", ctx.i18n.t("overview.actions.restart"), () => restartShell(), { id: "overview-restart" }),
      action("external", ctx.i18n.t("overview.actions.changelog"), () => openChangelog(), {
        id: "overview-changelog",
      }),
      action("power", ctx.i18n.t("overview.actions.quit"), () => quit(), { id: "overview-quit", danger: true }),
    );

    card.body.append(grid);

    const note = el("p", "panel__note", lastCheck ?? ctx.i18n.t("overview.actions.hint"));
    note.id = "overview-note";
    card.body.append(note);
    return card.el;
  };

  const render = (): void => {
    const grid = el("div", "overview__grid");
    grid.append(renderComponents(), renderActions());
    root.replaceChildren(renderHeader(), renderMetrics(), renderWinStrip(), grid);
    focus.restore();
  };

  render();

  return {
    el: root,
    setMetrics: updateMetrics,
    async activate() {
      if (!activated) {
        activated = true;
      }

      await ctx.refreshShell();
      // The strip is only useful when it is current; a host without `windows.*` keeps it at “no data”.
      await ctx.refreshWindows();
      render();
    },
    render,
  };
}
