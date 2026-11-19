import type { AppContext, Screen } from "../context";
import type { CallResult } from "../host";
import type { MessageKey } from "../i18n";
import { button, el, panel, pill, setBusy, skeletonPanel, switchControl } from "../components/controls";
import { icon, type IconName } from "../components/icons";
import { resetSectionCard } from "../components/ResetSection";
import { showToast } from "../components/toast";
import {
  normalizeWindowsState,
  windowsToggleIds,
  windowsToggleMap,
  type WindowsState,
  type WindowsToggleId,
} from "../state";

/**
 * «Замена Windows» — the reversible hiding of Windows shell elements.
 *
 * The host owns the registry work and remembers every previous value, so this screen only ever
 * paints what `windows.getState` answered. `applied` is host truth: a row reads «Применено» only
 * when the host says that value is currently hidden, never because the user ticked the box.
 */

interface ToggleRow {
  id: WindowsToggleId;
  icon: IconName;
  name: MessageKey;
  desc: MessageKey;
}

const rows: readonly ToggleRow[] = [
  { id: "taskbar", icon: "taskbar", name: "winmode.taskbar", desc: "winmode.taskbarDesc" },
  {
    id: "desktopIcons",
    icon: "folder",
    name: "winmode.desktopIcons",
    desc: "winmode.desktopIconsDesc",
  },
  { id: "searchBox", icon: "launcher", name: "winmode.searchBox", desc: "winmode.searchBoxDesc" },
  { id: "taskView", icon: "monitor", name: "winmode.taskView", desc: "winmode.taskViewDesc" },
  { id: "widgets", icon: "cloud", name: "winmode.widgets", desc: "winmode.widgetsDesc" },
  { id: "copilot", icon: "sparkles", name: "winmode.copilot", desc: "winmode.copilotDesc" },
];

type Busy = WindowsToggleId | "master" | "restore" | null;

export function createWindowsModeScreen(ctx: AppContext): Screen {
  const t = (key: MessageKey, params?: Record<string, string | number>): string =>
    ctx.i18n.t(key, params);

  const root = document.createElement("section");
  root.className = "screen winmode-screen";
  root.dataset.screen = "winmode";

  let state: WindowsState | null = null;
  let loading = true;
  let unavailable = false;
  let failed = false;
  let busy: Busy = null;

  const appliedCount = (value: WindowsState | null): number =>
    value ? windowsToggleIds.filter((id) => value.applied[id]).length : 0;

  const adopt = (raw: unknown): void => {
    const next = normalizeWindowsState(raw);
    if (next) {
      state = next;
    }
  };

  const load = async (quiet = false): Promise<void> => {
    if (!quiet) {
      loading = true;
      unavailable = false;
      failed = false;
      render();
    }

    const result = await ctx.host.call<unknown>("windows.getState");
    loading = false;

    if (!result.ok) {
      state = null;
      unavailable = result.reason === "unavailable";
      failed = !unavailable;
      render();
      return;
    }

    unavailable = false;
    failed = false;
    adopt(result.data);
    if (!state) {
      // A host answered, but with nothing this reader recognises: say so instead of guessing.
      failed = true;
    }

    render();
  };

  const fail = (result: Extract<CallResult<unknown>, { ok: false }>): void => {
    if (result.reason === "unavailable") {
      unavailable = true;
      state = null;
    }

    ctx.notifyFailure(result);
    render();
  };

  const write = async (
    action: () => Promise<CallResult<unknown>>,
    key: Busy,
    focus: string,
  ): Promise<boolean> => {
    busy = key;
    render();

    const result = await action();
    busy = null;

    if (!result.ok) {
      fail(result);
      return false;
    }

    adopt(result.data);
    render();
    root.querySelector<HTMLElement>(`[data-focus="${focus}"]`)?.focus();
    return true;
  };

  const setMaster = (next: boolean): void => {
    const current = state;
    if (!current || busy !== null) {
      return;
    }

    void (async () => {
      const ok = await write(
        () =>
          ctx.host.call<unknown>("windows.set", {
            enabled: next,
            toggles: { ...current.toggles },
          }),
        "master",
        "master",
      );

      if (!ok) {
        return;
      }

      if (!state) {
        state = {
          enabled: next,
          toggles: { ...current.toggles },
          applied: next ? { ...current.toggles } : windowsToggleMap(false),
        };
        render();
        root.querySelector<HTMLElement>('[data-focus="master"]')?.focus();
      }

      showToast(t(next ? "winmode.enabledToast" : "winmode.disabledToast"), "success");
    })();
  };

  const setToggle = (id: WindowsToggleId, next: boolean): void => {
    const current = state;
    if (!current || busy !== null) {
      return;
    }

    const toggles = { ...current.toggles, [id]: next };

    void (async () => {
      const ok = await write(
        () =>
          ctx.host.call<unknown>("windows.set", {
            // Sending `enabled` explicitly keeps a selection tick from turning the mode on: while
            // the mode is off the boxes are only remembered for the next enable.
            enabled: current.enabled,
            toggles,
          }),
        id,
        id,
      );

      if (!ok) {
        return;
      }

      if (!state) {
        state = {
          enabled: current.enabled,
          toggles,
          applied: current.enabled ? { ...current.applied, [id]: next } : { ...current.applied },
        };
        render();
        root.querySelector<HTMLElement>(`[data-focus="${id}"]`)?.focus();
      }

      showToast(t("common.saved"), "success");
    })();
  };

  const restore = (): void => {
    if (!state || busy !== null || appliedCount(state) === 0) {
      return;
    }

    void (async () => {
      const ok = await write(() => ctx.host.call<unknown>("windows.restore"), "restore", "restore");
      if (!ok) {
        return;
      }

      if (!state) {
        state = {
          enabled: false,
          toggles: windowsToggleMap(false),
          applied: windowsToggleMap(false),
        };
        render();
      }

      // The restore button is disabled once nothing is applied, so focus moves to the master switch
      // instead of falling back to the body.
      root.querySelector<HTMLElement>('[data-focus="master"]')?.focus();
      showToast(t("winmode.restore.done"), "success");
    })();
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(el("h1", "screen__title", t("winmode.title")), el("p", "screen__subtitle", t("winmode.subtitle")));

    const aside = el("div", "screen__aside");
    if (state) {
      aside.append(pill(t(state.enabled ? "winmode.state.on" : "winmode.state.off"), state.enabled ? "on" : "neutral"));
    } else if (unavailable) {
      aside.append(pill(t("common.unavailable"), "warn"));
    }

    head.append(copy, aside);
    return head;
  };

  const renderIntro = (): HTMLElement => {
    const card = el("section", "card winmode__intro");
    const glyph = el("span", "winmode__introIcon");
    glyph.innerHTML = icon("shield", 20);

    const copy = el("div", "winmode__introCopy");
    copy.append(
      el("h2", "winmode__introTitle", t("winmode.intro.title")),
      el("p", "winmode__introText", t("winmode.intro.text")),
    );

    const warning = el("div", "callout winmode__warning");
    const warnIcon = el("span", "callout__icon");
    warnIcon.innerHTML = icon("info", 17);
    const warnCopy = el("div", "callout__copy");
    warnCopy.append(
      el("span", "callout__title", t("winmode.intro.warningTitle")),
      el("span", "callout__hint", t("winmode.intro.warning")),
    );
    warning.append(warnIcon, warnCopy);

    copy.append(warning);
    card.append(glyph, copy);
    return card;
  };

  const renderMaster = (): HTMLElement => {
    const current = state;
    const enabled = current?.enabled === true;
    const count = appliedCount(current);
    const total = windowsToggleIds.length;

    const master = switchControl({
      checked: enabled,
      label: t("winmode.master.action"),
      disabled: busy !== null,
      onToggle: (next) => setMaster(next),
    });
    master.id = "winmode-enabled";
    master.dataset.focus = "master";

    const card = panel({
      title: t("winmode.master.title"),
      icon: "monitor",
      actions: [master],
      className: "winmode__master",
    });

    card.body.append(el("p", "winmode__masterText", t("winmode.master.desc")));

    const restoreButton = button({
      label: busy === "restore" ? t("winmode.restoring") : t("winmode.restore"),
      variant: "secondary",
      size: "sm",
      icon: "refresh",
      onClick: () => restore(),
    });
    restoreButton.id = "winmode-restore";
    restoreButton.dataset.focus = "restore";
    restoreButton.disabled = busy !== null || count === 0;
    if (busy === "restore") {
      setBusy(restoreButton, true, t("winmode.restoring"));
    }

    const footer = el("div", "winmode__footer");
    const summary = el(
      "span",
      `winmode__summary${count === 0 ? " winmode__summary--idle" : ""}`,
      count === 0 ? t("winmode.summaryNone") : t("winmode.summary", { count, total }),
    );
    summary.setAttribute("aria-live", "polite");
    summary.id = "winmode-summary";

    footer.append(summary, restoreButton);
    card.body.append(footer);
    return card.el;
  };

  const renderList = (): HTMLElement => {
    const current = state;
    const card = panel({
      title: t("winmode.list.title"),
      hint: t("winmode.list.hint"),
      icon: "taskbar",
      className: "winmode__list",
    });

    const list = el("div", "winmode__rows");
    list.setAttribute("role", "group");
    list.setAttribute("aria-label", t("winmode.list.title"));

    for (const row of rows) {
      const item = el("div", "winmode__row");
      item.dataset.toggle = row.id;

      const glyph = el("span", "winmode__rowIcon");
      glyph.innerHTML = icon(row.icon, 17);

      const copy = el("span", "winmode__rowCopy");
      copy.append(el("span", "winmode__rowName", t(row.name)), el("span", "winmode__rowDesc", t(row.desc)));

      const marks = el("span", "winmode__rowMarks");
      const applied = current?.applied[row.id] === true;
      const badge = pill(t(applied ? "winmode.applied" : "winmode.unchanged"), applied ? "on" : "neutral");
      badge.classList.add("winmode__badge");
      marks.append(badge);

      const toggle = switchControl({
        checked: current?.toggles[row.id] === true,
        label: t(row.name),
        disabled: busy !== null,
        onToggle: (next) => setToggle(row.id, next),
      });
      toggle.dataset.focus = row.id;

      item.append(glyph, copy, marks, toggle);
      list.append(item);
    }

    card.body.append(list);
    return card.el;
  };

  const renderMessage = (title: string, body: string): HTMLElement => {
    const card = panel({ className: "winmode__message" });
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

  const renderReset = (): HTMLElement =>
    resetSectionCard({
      label: t("reset.defaults"),
      action: t("common.reset"),
      hint: t("reset.winmodeHint"),
      confirmTitle: t("reset.confirm.title"),
      confirmText: t("reset.confirm.text", { section: t("winmode.title") }),
      confirmAction: t("reset.confirm.action"),
      cancel: t("common.cancel"),
      id: "winmode-reset-defaults",
      onReset: async () => {
        const defaults: Record<string, boolean> = { enabled: false };
        for (const id of windowsToggleIds) {
          defaults[id] = false;
        }

        const saved = await ctx.resetSections([{ section: "windowsMode", values: defaults }]);
        if (!saved) {
          return false;
        }

        if (state && appliedCount(state) > 0) {
          const result = await ctx.host.call<unknown>("windows.restore");
          if (!result.ok) {
            ctx.notifyFailure(result);
            return false;
          }
        }

        await load(true);
        return true;
      },
    });

  const render = (): void => {
    if (loading) {
      const grid = el("div", "winmode__grid");
      grid.append(skeletonPanel(4), skeletonPanel(5));
      root.replaceChildren(renderHeader(), grid);
      return;
    }

    if (unavailable) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(t("winmode.unavailable.title"), t("winmode.unavailable.text")),
      );
      return;
    }

    if (failed || !state) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(t("winmode.failed.title"), t("winmode.failed.text")),
      );
      return;
    }

    const grid = el("div", "winmode__grid");
    const side = el("div", "winmode__side");
    side.append(renderIntro(), renderMaster());
    grid.append(side, renderList());
    root.replaceChildren(renderHeader(), grid, renderReset());
  };

  render();

  return {
    el: root,
    async activate() {
      await load();
    },
    render,
  };
}
