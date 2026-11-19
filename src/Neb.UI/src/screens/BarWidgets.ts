import type { AppContext, Screen } from "../context";
import type { MessageKey } from "../i18n";
import type { BarWidget, WidgetKind } from "../state";
import { configSetParams, createWidget, defaultTheme, defaultWidgets } from "../state";
import {
  button,
  el,
  emptyState,
  formatClock,
  formatDate,
  iconButton,
  panel,
  segmented,
  skeletonPanel,
  switchControl,
} from "../components/controls";
import { icon, type IconName } from "../components/icons";
import { resetSectionCard } from "../components/ResetSection";
import { showToast } from "../components/toast";

const kinds: ReadonlyArray<{ kind: WidgetKind; label: MessageKey; icon: IconName }> = [
  { kind: "clock", label: "bar.kind.clock", icon: "clock" },
  { kind: "cpu", label: "bar.kind.cpu", icon: "zap" },
  { kind: "ram", label: "bar.kind.ram", icon: "monitor" },
  { kind: "text", label: "bar.kind.text", icon: "tag" },
];

const kindLabel: Record<WidgetKind, MessageKey> = {
  clock: "bar.kind.clock",
  cpu: "bar.kind.cpu",
  ram: "bar.kind.ram",
  text: "bar.kind.text",
};

const kindIcon: Record<WidgetKind, IconName> = {
  clock: "clock",
  cpu: "zap",
  ram: "monitor",
  text: "tag",
};

function cloneWidgets(widgets: readonly BarWidget[]): BarWidget[] {
  return widgets.map((widget) => ({ ...widget }));
}

export function createBarScreen(ctx: AppContext): Screen {
  const root = document.createElement("section");
  root.className = "screen bar-screen";
  root.dataset.screen = "bar";

  let widgets: BarWidget[] = cloneWidgets(ctx.config().shell.widgets);
  let baseline = cloneWidgets(widgets);
  let loading = true;
  let state: "idle" | "saving" | "saved" | "failed" = "idle";
  let timer = 0;
  let saveSeq = 0;

  const saveLabel = (): { text: string; tone: "neutral" | "on" | "danger" } => {
    if (state === "saving") {
      return { text: ctx.i18n.t("common.saving"), tone: "neutral" };
    }

    if (state === "failed") {
      return { text: ctx.i18n.t("bar.saveFailed"), tone: "danger" };
    }

    if (state === "saved") {
      return { text: ctx.i18n.t("common.saved"), tone: "on" };
    }

    return { text: "", tone: "neutral" };
  };

  const updateStatus = (): void => {
    const chip = root.querySelector<HTMLElement>("#bar-status");
    if (!chip) {
      return;
    }

    const label = saveLabel();
    chip.textContent = label.text;
    chip.dataset.tone = label.tone;
    chip.hidden = label.text.length === 0;
  };

  const saveNow = async (): Promise<void> => {
    window.clearTimeout(timer);
    timer = 0;
    const snapshot = cloneWidgets(widgets);
    const seq = ++saveSeq;
    state = "saving";
    updateStatus();

    const result = await ctx.host.call<unknown>(
      "config.set",
      configSetParams("shell.bar.widgets", snapshot),
    );

    if (seq !== saveSeq) {
      return;
    }

    if (result.ok) {
      baseline = snapshot;
      ctx.setLocalConfig("shell.bar.widgets", cloneWidgets(snapshot));
      state = "saved";
      updateStatus();
      showToast(ctx.i18n.t("common.saved"), "success");
      return;
    }

    state = "failed";
    widgets = cloneWidgets(baseline);
    ctx.notifyFailure(result);
    render();
  };

  const scheduleSave = (): void => {
    state = "saving";
    updateStatus();
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      void saveNow();
    }, 450);
  };

  const renderPreview = (): void => {
    const slot = root.querySelector<HTMLElement>("#bar-preview");
    if (!slot) {
      return;
    }

    const bar = el("div", "bar-preview__bar");
    const brand = el("div", "bar-preview__brand");
    brand.append(el("span", "bar-preview__mark"), el("span", "bar-preview__name", "ProjectNeb"));
    bar.append(brand, el("span", "bar-preview__spacer"));

    const now = new Date();
    let shown = 0;
    for (const widget of widgets) {
      if (!widget.enabled) {
        continue;
      }
      shown += 1;
      const item = el("span", "bar-preview__widget");

      if (widget.kind === "clock") {
        item.append(el("span", "bar-preview__value", formatClock(now, widget.format, ctx.i18n.language)));
        if (widget.showDate) {
          item.append(el("span", "bar-preview__date", formatDate(now, ctx.i18n.language)));
        }
      } else if (widget.kind === "cpu") {
        item.append(
          el("span", "bar-preview__value", widget.label ? "CPU 7 %" : "7 %"),
        );
      } else if (widget.kind === "ram") {
        item.append(
          el("span", "bar-preview__value", widget.label ? "RAM 61 %" : "61 %"),
        );
      } else {
        item.append(el("span", "bar-preview__value", widget.text.trim() === "" ? "—" : widget.text));
      }

      bar.append(item);
    }

    bar.classList.toggle("bar-preview__bar--many", shown > 3);

    if (shown === 0) {
      bar.append(el("span", "bar-preview__empty", ctx.i18n.t("bar.previewEmpty")));
    }

    bar.style.height = `${Math.min(46, Math.max(30, ctx.config().theme.barHeight))}px`;
    slot.replaceChildren(bar);
  };

  const changed = (): void => {
    renderPreview();
    scheduleSave();
  };

  const addWidget = (kind: WidgetKind): void => {
    const widget = createWidget(kind);
    widgets.push(widget);
    state = "idle";
    render();
    window.setTimeout(() => {
      root.querySelector<HTMLElement>(`[data-widget="${widget.id}"]`)?.focus();
    }, 0);
    void saveNow();
  };

  const removeWidget = (id: string): void => {
    widgets = widgets.filter((widget) => widget.id !== id);
    render();
    void saveNow();
  };

  const moveWidget = (id: string, delta: -1 | 1): void => {
    const index = widgets.findIndex((widget) => widget.id === id);
    const next = index + delta;
    if (index < 0 || next < 0 || next >= widgets.length) {
      return;
    }

    const [widget] = widgets.splice(index, 1);
    if (!widget) {
      return;
    }

    widgets.splice(next, 0, widget);
    render();
    window.setTimeout(() => {
      const button = root.querySelector<HTMLElement>(
        `[data-widget="${id}"] [data-move="${delta === -1 ? "up" : "down"}"]`,
      );
      button?.focus();
    }, 0);
    void saveNow();
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", ctx.i18n.t("bar.title")),
      el("p", "screen__subtitle", ctx.i18n.t("bar.subtitle")),
    );

    const aside = el("div", "screen__aside");
    const status = el("span", "save-state");
    status.id = "bar-status";
    status.hidden = true;
    aside.append(status);

    head.append(copy, aside);
    return head;
  };

  const renderWidget = (widget: BarWidget, index: number): HTMLElement => {
    const card = el("article", `widget${widget.enabled ? "" : " widget--off"}`);
    card.dataset.widget = widget.id;
    card.tabIndex = -1;
    card.setAttribute("aria-label", ctx.i18n.t(kindLabel[widget.kind]));

    const grip = el("span", "widget__grip");
    grip.title = ctx.i18n.t("bar.drag");
    grip.setAttribute("aria-hidden", "true");
    grip.innerHTML = icon("grip", 16);
    grip.addEventListener("mousedown", () => {
      card.draggable = true;
    });
    grip.addEventListener("mouseup", () => {
      card.draggable = false;
    });

    card.addEventListener("dragstart", (event) => {
      if (!card.draggable || !event.dataTransfer) {
        event.preventDefault();
        return;
      }

      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData("text/plain", widget.id);
      card.classList.add("widget--dragging");
    });

    card.addEventListener("dragend", () => {
      card.draggable = false;
      card.classList.remove("widget--dragging");
      for (const node of root.querySelectorAll(".widget--drop")) {
        node.classList.remove("widget--drop");
      }
    });

    card.addEventListener("dragover", (event) => {
      event.preventDefault();
      event.stopPropagation();
      card.classList.add("widget--drop");
    });

    card.addEventListener("dragleave", () => {
      card.classList.remove("widget--drop");
    });

    card.addEventListener("drop", (event) => {
      event.preventDefault();
      event.stopPropagation();
      card.classList.remove("widget--drop");
      const draggedId = event.dataTransfer?.getData("text/plain") ?? "";
      if (!draggedId || draggedId === widget.id) {
        return;
      }

      const from = widgets.findIndex((entry) => entry.id === draggedId);
      const to = widgets.findIndex((entry) => entry.id === widget.id);
      if (from < 0 || to < 0) {
        return;
      }

      const [dragged] = widgets.splice(from, 1);
      if (!dragged) {
        return;
      }

      widgets.splice(to, 0, dragged);
      render();
      void saveNow();
    });

    const main = el("div", "widget__main");
    const head = el("div", "widget__head");
    const glyph = el("span", "widget__icon");
    glyph.innerHTML = icon(kindIcon[widget.kind], 15);
    head.append(glyph, el("span", "widget__kind", ctx.i18n.t(kindLabel[widget.kind])));

    if (!widget.enabled) {
      head.append(el("span", "pill pill--neutral", ctx.i18n.t("bar.hidden")));
    }

    const controls = el("div", "widget__controls");

    if (widget.kind === "clock") {
      const formatRow = el("div", "widget__field");
      formatRow.append(el("span", "field__label", ctx.i18n.t("bar.clockFormat")));
      formatRow.append(
        segmented<"24" | "12">({
          value: widget.format,
          label: ctx.i18n.t("bar.clockFormat"),
          items: [
            { value: "24", label: ctx.i18n.t("bar.clock24") },
            { value: "12", label: ctx.i18n.t("bar.clock12") },
          ],
          onChange: (format) => {
            widget.format = format;
            changed();
            for (const choice of formatRow.querySelectorAll<HTMLElement>(".segmented__btn")) {
              const active =
                choice.textContent ===
                ctx.i18n.t(format === "24" ? "bar.clock24" : "bar.clock12");
              choice.classList.toggle("segmented__btn--active", active);
              choice.setAttribute("aria-checked", String(active));
            }
          },
        }),
      );
      controls.append(formatRow);

      const dateRow = el("div", "widget__field widget__field--inline");
      dateRow.append(el("span", "field__label", ctx.i18n.t("bar.showDate")));
      dateRow.append(
        switchControl({
          checked: widget.showDate,
          label: ctx.i18n.t("bar.showDate"),
          onToggle: (checked) => {
            widget.showDate = checked;
            changed();
          },
        }),
      );
      controls.append(dateRow);
    }

    if (widget.kind === "cpu" || widget.kind === "ram") {
      const labelRow = el("div", "widget__field widget__field--inline");
      labelRow.append(el("span", "field__label", ctx.i18n.t("bar.showLabel")));
      labelRow.append(
        switchControl({
          checked: widget.label,
          label: ctx.i18n.t("bar.showLabel"),
          onToggle: (checked) => {
            widget.label = checked;
            changed();
          },
        }),
      );
      controls.append(labelRow);
    }

    if (widget.kind === "text") {
      const textRow = el("div", "widget__field");
      const label = el("label", "field__label", ctx.i18n.t("bar.textLabel"));
      label.htmlFor = `widget-text-${widget.id}`;
      const input = document.createElement("input");
      input.className = "input";
      input.id = `widget-text-${widget.id}`;
      input.value = widget.text;
      input.maxLength = 48;
      input.placeholder = ctx.i18n.t("bar.textPlaceholder");
      input.addEventListener("input", () => {
        widget.text = input.value;
        changed();
      });
      textRow.append(label, input);
      controls.append(textRow);
    }

    main.append(head, controls);

    const actions = el("div", "widget__actions");
    actions.append(
      switchControl({
        checked: widget.enabled,
        label: ctx.i18n.t("bar.enabled"),
        onToggle: (checked) => {
          widget.enabled = checked;
          card.classList.toggle("widget--off", !checked);
          const hidden = head.querySelector(".pill");
          if (!checked && !hidden) {
            head.append(el("span", "pill pill--neutral", ctx.i18n.t("bar.hidden")));
          } else if (checked && hidden) {
            hidden.remove();
          }

          changed();
        },
      }),
    );

    const up = iconButton("arrowUp", ctx.i18n.t("bar.moveUp"), () => moveWidget(widget.id, -1));
    up.dataset.move = "up";
    up.disabled = index === 0;
    const down = iconButton("arrowDown", ctx.i18n.t("bar.moveDown"), () => moveWidget(widget.id, 1));
    down.dataset.move = "down";
    down.disabled = index === widgets.length - 1;
    const remove = iconButton(
      "trash",
      ctx.i18n.t("bar.remove"),
      () => removeWidget(widget.id),
      "icon-btn--danger",
    );
    actions.append(up, down, remove);

    card.append(grip, main, actions);
    return card;
  };

  const renderAddMenu = (): HTMLElement => {
    const menu = el("details", "menu");
    const summary = el("summary", "menu__trigger");
    summary.append(el("span", "menu__plus", "+"), el("span", "", ctx.i18n.t("bar.add")));
    menu.append(summary);

    const list = el("div", "menu__list");
    for (const entry of kinds) {
      const item = el("button", "menu__item");
      item.type = "button";
      const glyph = el("span", "menu__icon");
      glyph.innerHTML = icon(entry.icon, 15);
      item.append(glyph, el("span", "", ctx.i18n.t(entry.label)));
      item.addEventListener("click", () => {
        menu.open = false;
        addWidget(entry.kind);
      });
      list.append(item);
    }

    menu.append(list);
    menu.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        menu.open = false;
        summary.focus();
      }
    });

    return menu;
  };

  const renderListCard = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("bar.widgets"),
      hint: ctx.i18n.t("bar.widgetsHint"),
      icon: "bar",
      actions: [renderAddMenu()],
      className: "bar-screen__list",
    });

    const count = el("div", "panel__subhead", ctx.i18n.t("bar.widgetCount", { count: widgets.length }));
    card.body.append(count);

    if (widgets.length === 0) {
      card.body.append(
        emptyState({
          icon: "bar",
          title: ctx.i18n.t("bar.emptyTitle"),
          text: ctx.i18n.t("bar.emptyText"),
          action: button({
            label: ctx.i18n.t("bar.emptyAction"),
            variant: "primary",
            icon: "plus",
            onClick: () => addWidget("clock"),
          }),
        }),
      );
      return card.el;
    }

    const list = el("div", "widget-list");
    widgets.forEach((widget, index) => {
      list.append(renderWidget(widget, index));
    });

    list.addEventListener("dragover", (event) => {
      if (event.target === list) {
        event.preventDefault();
      }
    });

    list.addEventListener("drop", (event) => {
      if (event.target !== list || !event.dataTransfer) {
        return;
      }

      event.preventDefault();
      const draggedId = event.dataTransfer.getData("text/plain");
      const from = widgets.findIndex((widget) => widget.id === draggedId);
      if (from < 0) {
        return;
      }

      const [dragged] = widgets.splice(from, 1);
      if (!dragged) {
        return;
      }

      widgets.push(dragged);
      render();
      void saveNow();
    });

    card.body.append(list);
    return card.el;
  };

  const renderDockAutohide = (): HTMLElement => {
    const row = el("div", "field-row field-row--inline bar-screen__dockRow");
    const copy = el("div", "field-row__copy");
    const label = el("span", "field__label", ctx.i18n.t("bar.dockAutoHide"));
    label.id = "bar-dock-auto-hide-label";
    copy.append(label, el("span", "field__hint", ctx.i18n.t("bar.dockAutoHideHint")));

    const dockSwitch = switchControl({
      id: "bar-dock-auto-hide",
      checked: ctx.config().shell.dockAutoHide,
      label: ctx.i18n.t("bar.dockAutoHide"),
      title: ctx.i18n.t("bar.dockAutoHideHint"),
      onToggle: (next) => {
        void (async () => {
          const saved = await ctx.saveConfig("shell.dockAutoHide", next);
          if (!saved) {
            return;
          }

          render();
          root.querySelector<HTMLElement>("#bar-dock-auto-hide")?.focus();
        })();
      },
    });

    row.append(copy, dockSwitch);
    return row;
  };

  const renderPreviewCard = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("bar.preview"),
      hint: ctx.i18n.t("bar.previewHint"),
      icon: "monitor",
      className: "bar-screen__preview",
    });

    const slot = el("div", "bar-preview");
    slot.id = "bar-preview";
    card.body.append(slot, renderDockAutohide());
    return card.el;
  };

  const renderReset = (): HTMLElement =>
    resetSectionCard({
      label: ctx.i18n.t("reset.defaults"),
      action: ctx.i18n.t("common.reset"),
      hint: ctx.i18n.t("reset.barHint"),
      confirmTitle: ctx.i18n.t("reset.confirm.title"),
      confirmText: ctx.i18n.t("reset.confirm.text", { section: ctx.i18n.t("bar.title") }),
      confirmAction: ctx.i18n.t("reset.confirm.action"),
      cancel: ctx.i18n.t("common.cancel"),
      id: "bar-reset-defaults",
      onReset: async () => {
        const defaults = defaultWidgets();
        const saved = await ctx.resetSections([
          {
            section: "shell",
            values: {
              barHeight: defaultTheme.barHeight,
              dockTileSize: defaultTheme.dockTileSize,
              dockAutoHide: true,
              widgets: defaults,
            },
          },
        ]);
        if (!saved) {
          return false;
        }

        widgets = cloneWidgets(defaults);
        baseline = cloneWidgets(defaults);
        ctx.setLocalConfig("shell.widgets", cloneWidgets(defaults));
        render();
        return true;
      },
    });

  const render = (): void => {
    if (loading) {
      const grid = el("div", "bar-screen__grid");
      grid.append(skeletonPanel(4), skeletonPanel(3));
      root.replaceChildren(renderHeader(), grid);
      return;
    }

    const grid = el("div", "bar-screen__grid");
    grid.append(renderListCard(), renderPreviewCard());
    root.replaceChildren(renderHeader(), grid, renderReset());
    renderPreview();
    updateStatus();
  };

  const activate = async (): Promise<void> => {
    widgets = cloneWidgets(ctx.config().shell.widgets);
    baseline = cloneWidgets(widgets);
    loading = false;
    state = "idle";
    render();
  };

  activate();

  return {
    el: root,
    activate,
    deactivate() {
      if (timer !== 0) {
        void saveNow();
      }
    },
    render,
  };
}
