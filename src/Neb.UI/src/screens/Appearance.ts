import type { AppContext, Screen } from "../context";
import type { MessageKey } from "../i18n";
import type { ThemeColors, ThemeSettings } from "../state";
import { cloneTheme, defaultTheme, isHexColour, normalizeTheme } from "../state";
import { button, el, panel, skeletonPanel, switchControl } from "../components/controls";
import { resetSectionCard } from "../components/ResetSection";
import { showToast } from "../components/toast";

const colourFields: ReadonlyArray<{ field: keyof ThemeColors; label: MessageKey }> = [
  { field: "background", label: "appearance.colors.background" },
  { field: "surface", label: "appearance.colors.surface" },
  { field: "stroke", label: "appearance.colors.stroke" },
  { field: "text", label: "appearance.colors.text" },
  { field: "textDim", label: "appearance.colors.textDim" },
  { field: "accent", label: "appearance.colors.accent" },
  { field: "accent2", label: "appearance.colors.accent2" },
];

const fontOptions: ReadonlyArray<{ value: string; label: MessageKey }> = [
  { value: "Onest", label: "appearance.font.onest" },
  { value: "JetBrains Mono", label: "appearance.font.mono" },
  { value: "system", label: "appearance.font.system" },
];

function fontStack(font: string): string {
  if (font === "JetBrains Mono") {
    return '"JetBrains Mono", monospace';
  }

  if (font === "system") {
    return "system-ui, sans-serif";
  }

  return "Onest, system-ui, sans-serif";
}

function readThemes(raw: unknown): ThemeSettings[] {
  const list = Array.isArray(raw)
    ? raw
    : typeof raw === "object" && raw !== null && Array.isArray((raw as { themes?: unknown }).themes)
      ? (raw as { themes: unknown[] }).themes
      : [];

  return list
    .map((entry) => normalizeTheme(entry, defaultTheme))
    .filter((theme) => theme.id.length > 0);
}

function swatch(theme: ThemeSettings): string {
  return `radial-gradient(circle at 32% 26%, ${theme.colors.accent} 0 34%, ${theme.colors.background} 100%)`;
}

export function createAppearanceScreen(ctx: AppContext): Screen {
  const root = document.createElement("section");
  root.className = "screen appearance";
  root.dataset.screen = "appearance";

  let draft: ThemeSettings = cloneTheme(ctx.config().theme);
  let baseline: ThemeSettings = cloneTheme(draft);
  let themes: ThemeSettings[] = [];
  let loading = false;
  let saving = false;
  let invalid = new Set<keyof ThemeColors>();
  let preview: HTMLElement | null = null;
  let dirtyChip: HTMLElement | null = null;
  let saveButton: HTMLButtonElement | null = null;
  let resetButton: HTMLButtonElement | null = null;

  const dirty = (): boolean => JSON.stringify(draft) !== JSON.stringify(baseline);

  const setDirtyChrome = (): void => {
    if (dirtyChip) {
      dirtyChip.hidden = !dirty();
    }

    if (saveButton) {
      saveButton.disabled = saving || loading || invalid.size > 0;
      saveButton.classList.toggle("is-busy", saving);
      const label = saveButton.querySelector(".btn__label");
      if (label) {
        label.textContent = ctx.i18n.t("appearance.save");
      }
    }

    if (resetButton) {
      resetButton.disabled = !dirty() || saving;
    }
  };

  const applyPreview = (): void => {
    if (!preview) {
      return;
    }

    const colors = draft.colors;
    preview.style.setProperty("--pv-bg", colors.background);
    preview.style.setProperty("--pv-surface", colors.surface);
    preview.style.setProperty("--pv-stroke", colors.stroke);
    preview.style.setProperty("--pv-text", colors.text);
    preview.style.setProperty("--pv-dim", colors.textDim);
    preview.style.setProperty("--pv-accent", colors.accent);
    preview.style.setProperty("--pv-accent2", colors.accent2);
    preview.style.setProperty("--pv-opacity", `${draft.opacity}%`);
    preview.style.setProperty("--pv-radius", `${draft.radius}px`);
    preview.style.setProperty("--pv-font", fontStack(draft.font));
    preview.style.setProperty("--pv-bar", `${draft.barHeight}px`);
    preview.style.setProperty("--pv-tile", `${Math.max(24, Math.round(draft.dockTileSize * 0.72))}px`);

    const name = preview.querySelector(".theme-preview__name");
    if (name) {
      name.textContent = draft.name;
    }

    const clock = preview.querySelector(".theme-preview__clock");
    if (clock) {
      clock.textContent = new Intl.DateTimeFormat(
        ctx.i18n.language === "en" ? "en-US" : "ru-RU",
        { hour: "2-digit", minute: "2-digit" },
      ).format(new Date());
    }

    const date = preview.querySelector(".theme-preview__date");
    if (date) {
      date.textContent = new Intl.DateTimeFormat(
        ctx.i18n.language === "en" ? "en-US" : "ru-RU",
        { weekday: "short", day: "numeric", month: "long" },
      ).format(new Date());
    }
  };

  const setColour = (field: keyof ThemeColors, value: string, input: HTMLInputElement): void => {
    if (!isHexColour(value)) {
      invalid.add(field);
      input.setAttribute("aria-invalid", "true");
      const error = input.closest(".color-field")?.querySelector<HTMLElement>(".field__error");
      if (error) {
        error.hidden = false;
      }

      setDirtyChrome();
      return;
    }

    invalid.delete(field);
    input.removeAttribute("aria-invalid");
    const error = input.closest(".color-field")?.querySelector<HTMLElement>(".field__error");
    if (error) {
      error.hidden = true;
    }

    draft.colors[field] = value.toUpperCase();
    applyPreview();
    setDirtyChrome();
  };

  const setTheme = (theme: ThemeSettings): void => {
    draft = cloneTheme(theme);
    invalid = new Set();
    render();
  };

  const save = async (): Promise<void> => {
    if (saving || invalid.size > 0) {
      if (invalid.size > 0) {
        showToast(ctx.i18n.t("appearance.invalidColour"), "error");
      }

      return;
    }

    if (draft.name.trim() === "") {
      draft.name = ctx.i18n.t("appearance.custom");
    }

    saving = true;
    setDirtyChrome();

    const result = await ctx.host.call<unknown>("themes.save", {
      ...draft,
      theme: { ...draft, colors: { ...draft.colors } },
    });

    saving = false;
    if (!result.ok) {
      ctx.notifyFailure(result);
      setDirtyChrome();
      return;
    }

    baseline = cloneTheme(draft);
    ctx.setLocalConfig("theme", cloneTheme(draft));
    ctx.setThemeName(draft.name);
    setDirtyChrome();
    showToast(ctx.i18n.t("appearance.saved"), "success");
    render();

    const reload = await ctx.host.call<unknown>("shell.reload");
    if (!reload.ok) {
      showToast(ctx.i18n.t("appearance.saveReloadFailed"), "error");
    }
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", ctx.i18n.t("appearance.title")),
      el("p", "screen__subtitle", ctx.i18n.t("appearance.subtitle")),
    );

    const aside = el("div", "screen__aside");
    dirtyChip = el("span", "pill pill--warn", ctx.i18n.t("appearance.unsaved"));
    dirtyChip.hidden = !dirty();
    dirtyChip.id = "appearance-dirty";

    resetButton = button({
      label: ctx.i18n.t("common.reset"),
      variant: "ghost",
      size: "sm",
      title: ctx.i18n.t("appearance.resetHint"),
      onClick: () => {
        draft = cloneTheme(baseline);
        invalid = new Set();
        render();
      },
    });
    resetButton.id = "appearance-reset";

    saveButton = button({
      label: ctx.i18n.t("appearance.save"),
      variant: "primary",
      size: "sm",
      icon: "check",
      onClick: () => void save(),
    });
    saveButton.id = "appearance-save";

    aside.append(dirtyChip, resetButton, saveButton);
    head.append(copy, aside);
    return head;
  };

  const renderPresets = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("appearance.presets"),
      hint: ctx.i18n.t("appearance.presetsHint"),
      icon: "palette",
      className: "appearance__presets",
    });

    const list = el("div", "presets");
    for (const theme of themes) {
      const active = theme.id === draft.id;
      const choice = el("button", `preset${active ? " preset--active" : ""}`);
      choice.type = "button";
      choice.dataset.theme = theme.id;
      choice.setAttribute("aria-pressed", String(active));

      const dot = el("span", "preset__swatch");
      dot.style.background = swatch(theme);
      const name = el("span", "preset__name", theme.name);
      choice.append(dot, name);
      choice.addEventListener("click", () => setTheme(theme));
      list.append(choice);
    }

    const nameField = el("label", "name-field");
    nameField.append(el("span", "field__label", ctx.i18n.t("appearance.name")));
    const nameInput = document.createElement("input");
    nameInput.className = "input";
    nameInput.id = "appearance-name";
    nameInput.value = draft.name;
    nameInput.maxLength = 48;
    nameInput.spellcheck = false;
    nameInput.addEventListener("input", () => {
      draft.name = nameInput.value;
      const title = preview?.querySelector(".theme-preview__name");
      if (title) {
        title.textContent = draft.name;
      }

      setDirtyChrome();
    });
    nameField.append(nameInput);

    card.body.append(list, nameField);
    return card.el;
  };

  const renderPalette = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("appearance.palette"),
      hint: ctx.i18n.t("appearance.paletteHint"),
      icon: "customize",
      className: "appearance__palette",
    });

    const grid = el("div", "colour-grid");
    for (const entry of colourFields) {
      const field = el("div", "color-field");
      field.dataset.field = entry.field;

      const label = el("label", "field__label", ctx.i18n.t(entry.label));
      label.htmlFor = `colour-${entry.field}`;

      const controls = el("div", "color-field__controls");

      const picker = document.createElement("input");
      picker.type = "color";
      picker.className = "color-field__swatch";
      picker.value = draft.colors[entry.field];
      picker.setAttribute("aria-label", ctx.i18n.t(entry.label));

      const hex = document.createElement("input");
      hex.type = "text";
      hex.id = `colour-${entry.field}`;
      hex.className = "input input--mono color-field__hex";
      hex.value = draft.colors[entry.field].toUpperCase();
      hex.maxLength = 7;
      hex.spellcheck = false;
      hex.autocomplete = "off";
      hex.setAttribute("aria-label", `${ctx.i18n.t(entry.label)} #RRGGBB`);

      picker.addEventListener("input", () => {
        hex.value = picker.value.toUpperCase();
        setColour(entry.field, picker.value, hex);
      });

      hex.addEventListener("input", () => {
        const value = hex.value.trim();
        if (isHexColour(value)) {
          picker.value = value.toLowerCase();
        }

        setColour(entry.field, value, hex);
      });

      hex.addEventListener("blur", () => {
        if (isHexColour(hex.value.trim())) {
          hex.value = hex.value.trim().toUpperCase();
        }
      });

      controls.append(picker, hex);
      const error = el("span", "field__error", ctx.i18n.t("appearance.invalidColour"));
      error.hidden = true;
      field.append(label, controls, error);
      grid.append(field);
    }

    card.body.append(grid);
    return card.el;
  };

  const sliderRow = (options: {
    id: string;
    label: string;
    value: number;
    min: number;
    max: number;
    step: number;
    suffix: "appearance.radiusValue" | "appearance.opacityValue" | null;
    onChange(value: number): void;
  }): HTMLElement => {
    const row = el("div", "range-field");
    const head = el("div", "range-field__head");
    head.append(el("label", "field__label", options.label));

    const output = el("span", "range-field__value");
    const format = (): string => {
      if (!options.suffix) {
        return String(options.value);
      }

      return ctx.i18n.t(options.suffix, { value: options.value });
    };
    output.textContent = format();

    const input = document.createElement("input");
    input.type = "range";
    input.className = "range";
    input.id = options.id;
    input.min = String(options.min);
    input.max = String(options.max);
    input.step = String(options.step);
    input.value = String(options.value);
    input.addEventListener("input", () => {
      const value = Number(input.value);
      output.textContent = options.suffix
        ? ctx.i18n.t(options.suffix, { value })
        : String(value);

      options.onChange(value);
      applyPreview();
      setDirtyChrome();
    });

    head.append(output);
    row.append(head, input);
    return row;
  };

  const renderShape = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("appearance.shape"),
      hint: ctx.i18n.t("appearance.shapeHint"),
      icon: "sparkles",
      className: "appearance__shape",
    });

    card.body.append(
      sliderRow({
        id: "theme-radius",
        label: ctx.i18n.t("appearance.radius"),
        value: draft.radius,
        min: 0,
        max: 28,
        step: 1,
        suffix: "appearance.radiusValue",
        onChange: (value) => {
          draft.radius = value;
        },
      }),
      sliderRow({
        id: "theme-opacity",
        label: ctx.i18n.t("appearance.opacity"),
        value: draft.opacity,
        min: 20,
        max: 100,
        step: 1,
        suffix: "appearance.opacityValue",
        onChange: (value) => {
          draft.opacity = value;
        },
      }),
    );

    const fontRow = el("div", "field-row");
    const fontLabel = el("label", "field__label", ctx.i18n.t("appearance.font"));
    fontLabel.htmlFor = "theme-font";
    const select = document.createElement("select");
    select.className = "select";
    select.id = "theme-font";
    for (const font of fontOptions) {
      const option = document.createElement("option");
      option.value = font.value;
      option.textContent = ctx.i18n.t(font.label);
      option.selected = font.value === draft.font;
      select.append(option);
    }

    select.addEventListener("change", () => {
      draft.font = select.value;
      applyPreview();
      setDirtyChrome();
    });

    fontRow.append(fontLabel, select);

    const animationsRow = el("div", "field-row field-row--inline");
    const animationsCopy = el("div", "field-row__copy");
    const animationsLabel = el("span", "field__label", ctx.i18n.t("appearance.animations"));
    animationsLabel.id = "theme-animations-label";
    const animationsHint = el("span", "field__hint", ctx.i18n.t("appearance.animationsHint"));
    animationsCopy.append(animationsLabel, animationsHint);

    const animationsSwitch = switchControl({
      id: "theme-animations",
      checked: ctx.config().animations.enabled,
      label: ctx.i18n.t("appearance.animations"),
      title: ctx.i18n.t("appearance.animationsHint"),
      onToggle: (next) => {
        void (async () => {
          const saved = await ctx.saveConfig("animations.enabled", next);
          if (!saved) {
            return;
          }

          render();
          root.querySelector<HTMLElement>("#theme-animations")?.focus();
        })();
      },
    });
    animationsRow.append(animationsCopy, animationsSwitch);

    const geometry = el("div", "panel__subhead", ctx.i18n.t("appearance.barSection"));
    card.body.append(
      fontRow,
      animationsRow,
      geometry,
      sliderRow({
        id: "theme-bar-height",
        label: ctx.i18n.t("appearance.barHeight"),
        value: draft.barHeight,
        min: 28,
        max: 64,
        step: 2,
        suffix: null,
        onChange: (value) => {
          draft.barHeight = value;
        },
      }),
      sliderRow({
        id: "theme-dock-tile",
        label: ctx.i18n.t("appearance.dockTile"),
        value: draft.dockTileSize,
        min: 40,
        max: 80,
        step: 2,
        suffix: null,
        onChange: (value) => {
          draft.dockTileSize = value;
        },
      }),
    );

    return card.el;
  };

  const renderPreview = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("appearance.preview"),
      hint: ctx.i18n.t("appearance.previewHint"),
      icon: "monitor",
      className: "appearance__previewCard",
    });

    const scene = el("div", "theme-preview");
    scene.setAttribute("role", "img");
    scene.setAttribute("aria-label", ctx.i18n.t("appearance.preview.desktop", { name: draft.name }));

    const desktop = el("div", "theme-preview__desktop");
    const icons = el("div", "theme-preview__icons");
    for (let index = 0; index < 3; index += 1) {
      const tile = el("span", "theme-preview__icon");
      tile.style.width = `${Math.round(draft.dockTileSize * 0.6)}px`;
      tile.style.height = `${Math.round(draft.dockTileSize * 0.6)}px`;
      icons.append(tile);
    }

    const bar = el("div", "theme-preview__bar");
    const barBrand = el("div", "theme-preview__brand");
    barBrand.append(el("span", "theme-preview__mark"), el("span", "theme-preview__name", draft.name));

    const barRight = el("div", "theme-preview__barRight");
    barRight.append(
      el("span", "theme-preview__pill", ctx.i18n.t("appearance.preview.cpu")),
      el("span", "theme-preview__pill", ctx.i18n.t("appearance.preview.ram")),
      el("span", "theme-preview__clock", "--:--"),
      el("span", "theme-preview__date", ""),
    );
    bar.append(barBrand, barRight);

    const dock = el("div", "theme-preview__dock");
    dock.setAttribute("aria-label", ctx.i18n.t("appearance.preview.apps"));
    for (let index = 0; index < 5; index += 1) {
      dock.append(el("span", "theme-preview__app"));
    }

    const sample = el("div", "theme-preview__card");
    sample.append(
      el("span", "theme-preview__cardTitle", ctx.i18n.t("app.name")),
      el("span", "theme-preview__cardLine"),
      el("span", "theme-preview__cardLine theme-preview__cardLine--short"),
    );

    desktop.append(icons, bar, sample, dock);
    scene.append(desktop, el("span", "theme-preview__caption", ctx.i18n.t("appearance.previewHint")));
    card.body.append(scene);

    preview = scene;
    applyPreview();
    return card.el;
  };

  const renderReset = (): HTMLElement =>
    resetSectionCard({
      label: ctx.i18n.t("reset.defaults"),
      action: ctx.i18n.t("common.reset"),
      hint: ctx.i18n.t("reset.appearanceHint"),
      confirmTitle: ctx.i18n.t("reset.confirm.title"),
      confirmText: ctx.i18n.t("reset.confirm.text", { section: ctx.i18n.t("appearance.title") }),
      confirmAction: ctx.i18n.t("reset.confirm.action"),
      cancel: ctx.i18n.t("common.cancel"),
      id: "appearance-reset-defaults",
      onReset: async () => {
        // The rich palette lives in the host's theme store (`config.theme` only carries the
        // active theme id), so the defaults are written through the same `themes.save` the editor
        // uses; the motion switch is a plain config section and merges normally.
        const themeResult = await ctx.host.call<unknown>("themes.save", {
          ...cloneTheme(defaultTheme),
          theme: cloneTheme(defaultTheme),
        });
        if (!themeResult.ok) {
          ctx.notifyFailure(themeResult);
          return false;
        }

        ctx.setLocalConfig("theme", cloneTheme(defaultTheme));
        ctx.setThemeName(defaultTheme.name);

        const saved = await ctx.resetSections([
          { section: "animations", values: { enabled: true, speed: 1 } },
        ]);
        if (!saved) {
          return false;
        }

        draft = cloneTheme(defaultTheme);
        baseline = cloneTheme(draft);
        invalid = new Set();
        render();
        return true;
      },
    });

  const render = (): void => {
    if (loading) {
      const wrap = el("div", "appearance__grid");
      wrap.append(skeletonPanel(6), skeletonPanel(5));
      root.replaceChildren(renderHeader(), wrap);
      setDirtyChrome();
      return;
    }

    const grid = el("div", "appearance__grid");
    const left = el("div", "appearance__editor");
    left.append(renderPresets(), renderPalette());

    const right = el("div", "appearance__side");
    right.append(renderPreview(), renderShape());

    grid.append(left, right);
    root.replaceChildren(renderHeader(), grid, renderReset());
    setDirtyChrome();
  };

  const activate = async (): Promise<void> => {
    loading = true;
    render();

    const result = await ctx.host.call<unknown>("themes.list");
    loading = false;

    if (result.ok) {
      const raw = result.data as { active?: unknown } | null;
      themes = readThemes(result.data);

      const activeId =
        typeof raw === "object" && raw !== null && typeof raw.active === "string"
          ? raw.active
          : ctx.config().theme.id;

      const configTheme = ctx.config().theme;
      const active = themes.find((theme) => theme.id === activeId) ?? configTheme;
      if (!themes.some((theme) => theme.id === active.id)) {
        themes = [active, ...themes];
      }

      draft = cloneTheme(active);
      baseline = cloneTheme(draft);
      ctx.setThemeName(draft.name);
    } else {
      // The editor still works with the config copy; only the preset list is missing.
      themes = themes.length > 0 ? themes : [ctx.config().theme];
      draft = cloneTheme(ctx.config().theme);
      baseline = cloneTheme(draft);
    }

    invalid = new Set();
    render();
  };

  return { el: root, activate, render };
}
