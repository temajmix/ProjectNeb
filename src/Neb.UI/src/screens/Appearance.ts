import type { AppContext, Screen } from "../context";
import type { MessageKey } from "../i18n";
import type { ThemeColors, ThemeSettings } from "../state";
import { cloneTheme, defaultTheme, isHexColour, normalizeTheme } from "../state";
import { button, el, panel, skeletonPanel, switchControl } from "../components/controls";
import { resetSectionCard } from "../components/ResetSection";
import { showToast } from "../components/toast";
import { watchFocus } from "../components/focus";

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

/**
 * The gallery's own copy. `i18n/*` belongs to another slice, so the two new sentences live here
 * and follow `ctx.i18n.language`; everything that already has a key reuses it.
 */
const copy = {
  ru: {
    galleryHint: "Шесть готовых образов. Применение перекрашивает бар и док сразу.",
    apply: "Применить",
    applied: "Применена",
    appliedToast: "Тема «{name}» применена",
    applyFailed: "Не удалось применить тему",
    customTitle: "Свои темы",
    customHint: "Сохранённые из редактора ниже.",
    customDescription: "Своя палитра без описания.",
    presetDescription: "Готовая палитра ProjectNeb.",
  },
  en: {
    galleryHint: "Six ready-made looks. Applying one repaints the bar and the dock immediately.",
    apply: "Apply",
    applied: "Applied",
    appliedToast: "Theme “{name}” applied",
    applyFailed: "The theme could not be applied",
    customTitle: "My themes",
    customHint: "Saved from the editor below.",
    customDescription: "A custom palette without a description.",
    presetDescription: "A ready-made ProjectNeb palette.",
  },
} as const;

type CopyKey = keyof (typeof copy)["ru"];

/**
 * Russian descriptions for the six compiled-in presets. The package keeps English, which is the
 * right default for an exported theme; the gallery reads the active language here.
 */
const presetText: Record<string, { ru: string; en: string }> = {
  "nebula-glass": {
    ru: "Фирменный вид ProjectNeb: глубокое фиолетовое стекло и мягкий лавандовый акцент.",
    en: "The shipped ProjectNeb look: deep violet glass with a soft lavender accent.",
  },
  hyprland: {
    ru: "Плоский и тонкий, почти без скруглений, с контрастным бирюзовым акцентом.",
    en: "Flat, thin and nearly square, with one high-contrast teal accent.",
  },
  macos: {
    ru: "Компактная матовая панель с ярким системно-синим акцентом.",
    en: "A compact frosted bar with a bright system-blue accent.",
  },
  "minimal-dark": {
    ru: "Почти чёрный, квадратный и тихий: без градиентов и лишнего.",
    en: "Near-black, square and quiet: no gradients, no fuss.",
  },
  cyberpunk: {
    ru: "Маджента и циан на чистом чёрном — для неоновых вечеров.",
    en: "Magenta and cyan on pure black, built for neon nights.",
  },
  "windows-clean": {
    ru: "Нейтральный серый и синий Windows 11, близко к системному оформлению.",
    en: "Neutral grey and Windows blue, close to the stock Windows 11 chrome.",
  },
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** A listed theme: what the editor edits plus the gallery's description and origin. */
interface ThemeEntry extends ThemeSettings {
  description: string;
  builtin: boolean;
}

/** The package stores opacity as `0.4..=1`, the editor speaks percent. */
function opacityPercent(value: unknown): number | undefined {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return undefined;
  }

  return value <= 1 ? Math.round(value * 100) : value;
}

function readThemeEntry(raw: unknown, fallback: ThemeSettings): ThemeEntry | null {
  if (!isRecord(raw)) {
    return null;
  }

  const tokens = isRecord(raw.tokens) ? raw.tokens : raw;
  const theme = normalizeTheme(
    {
      ...tokens,
      id: typeof raw.id === "string" ? raw.id : tokens.id,
      name: typeof raw.name === "string" ? raw.name : tokens.name,
      opacity: opacityPercent(tokens.opacity) ?? fallback.opacity,
      dockTileSize:
        typeof tokens.dockTile === "number" ? tokens.dockTile : tokens.dockTileSize,
    },
    fallback,
  );

  if (theme.id.length === 0) {
    return null;
  }

  return {
    ...theme,
    description: typeof raw.description === "string" ? raw.description.trim() : "",
    builtin: raw.builtin === true || raw.builtIn === true,
  };
}

function readThemes(raw: unknown): ThemeEntry[] {
  const list = Array.isArray(raw)
    ? raw
    : isRecord(raw) && Array.isArray(raw.themes)
      ? raw.themes
      : [];

  return list
    .map((entry) => readThemeEntry(entry, defaultTheme))
    .filter((theme): theme is ThemeEntry => theme !== null);
}

function fontStack(font: string): string {
  const mono = /mono|cascadia|consolas/i.test(font);
  if (mono) {
    return `"${font}", "JetBrains Mono", monospace`;
  }

  if (font === "system") {
    return "system-ui, sans-serif";
  }

  if (font.length > 0 && font !== "Onest") {
    return `"${font}", Onest, system-ui, sans-serif`;
  }

  return "Onest, system-ui, sans-serif";
}

export function createAppearanceScreen(ctx: AppContext): Screen {
  const root = document.createElement("section");
  root.className = "screen appearance";
  root.dataset.screen = "appearance";

  const t = (key: CopyKey, params?: Record<string, string | number>): string => {
    const template = copy[ctx.i18n.language][key];
    if (!params) {
      return template;
    }

    return template.replace(/\{(\w+)\}/g, (match, name: string) =>
      params[name] === undefined ? match : String(params[name]),
    );
  };

  let draft: ThemeSettings = cloneTheme(ctx.config().theme);
  let baseline: ThemeSettings = cloneTheme(draft);
  let draftDescription = "";
  let themes: ThemeEntry[] = [];
  let activeId = ctx.config().theme.id;
  let applyingId: string | null = null;
  let loading = false;
  let saving = false;
  let invalid = new Set<keyof ThemeColors>();
  let preview: HTMLElement | null = null;
  let dirtyChip: HTMLElement | null = null;
  let saveButton: HTMLButtonElement | null = null;
  let resetButton: HTMLButtonElement | null = null;
  const focus = watchFocus(root);

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
      input.setAttribute("aria-errormessage", `${input.id}-error`);
      const error = input.closest(".color-field")?.querySelector<HTMLElement>(".field__error");
      if (error) {
        error.hidden = false;
      }

      setDirtyChrome();
      return;
    }

    invalid.delete(field);
    input.removeAttribute("aria-invalid");
    input.removeAttribute("aria-errormessage");
    const error = input.closest(".color-field")?.querySelector<HTMLElement>(".field__error");
    if (error) {
      error.hidden = true;
    }

    draft.colors[field] = value.toUpperCase();
    applyPreview();
    setDirtyChrome();
  };

  const mergeTheme = (theme: ThemeEntry): void => {
    const index = themes.findIndex((known) => known.id === theme.id);
    if (index >= 0) {
      themes = themes.map((known) => (known.id === theme.id ? theme : known));
    } else {
      themes = [...themes, theme];
    }
  };

  const adoptTheme = (theme: ThemeEntry): void => {
    draft = cloneTheme(theme);
    baseline = cloneTheme(draft);
    draftDescription = theme.description;
    invalid = new Set();
    applyPreview();
    setDirtyChrome();
  };

  const applyTheme = async (theme: ThemeEntry): Promise<void> => {
    if (applyingId !== null || saving) {
      return;
    }

    applyingId = theme.id;
    render();

    const result = await ctx.host.call<unknown>("themes.apply", {
      id: theme.id,
      themeId: theme.id,
    });
    applyingId = null;

    if (!result.ok) {
      ctx.notifyFailure(result);
      render();
      return;
    }

    const payload = isRecord(result.data) ? result.data : {};
    activeId = typeof payload.id === "string" ? payload.id : theme.id;
    mergeTheme(theme);
    adoptTheme(theme);
    ctx.setLocalConfig("theme", cloneTheme(theme));
    ctx.setThemeName(theme.name);
    showToast(t("appliedToast", { name: theme.name }), "success");
    render();
  };

  const save = async (): Promise<void> => {
    if (saving || invalid.size > 0) {
      if (invalid.size > 0) {
        showToast(ctx.i18n.t("appearance.invalidColour"), "error");
        const first = invalid.values().next().value as keyof ThemeColors | undefined;
        if (first) {
          root.querySelector<HTMLInputElement>(`#colour-${first}`)?.focus();
        }
      }

      return;
    }

    if (draft.name.trim() === "") {
      draft.name = ctx.i18n.t("appearance.custom");
    }

    saving = true;
    setDirtyChrome();

    const document = {
      ...draft,
      description: draftDescription,
      theme: {
        ...draft,
        description: draftDescription,
        colors: { ...draft.colors },
      },
    };
    const result = await ctx.host.call<unknown>("themes.save", document);

    saving = false;
    if (!result.ok) {
      ctx.notifyFailure(result);
      setDirtyChrome();
      return;
    }

    const payload = isRecord(result.data) ? result.data : {};
    const saved = readThemeEntry(payload.theme, draft) ?? {
      ...cloneTheme(draft),
      description: draftDescription,
      builtin: false,
    };
    activeId = typeof payload.active === "string" ? payload.active : saved.id;
    mergeTheme(saved);
    adoptTheme(saved);
    ctx.setLocalConfig("theme", cloneTheme(saved));
    ctx.setThemeName(saved.name);
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
    dirtyChip.setAttribute("role", "status");

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

  const themeCard = (theme: ThemeEntry): HTMLElement => {
    const active = theme.id === activeId;
    const busy = applyingId === theme.id;
    const card = el("article", `preset-card${active ? " preset-card--active" : ""}`);
    card.dataset.theme = theme.id;
    card.setAttribute("role", "listitem");

    const preview = el("div", "preset-card__preview");
    preview.setAttribute("aria-hidden", "true");
    preview.style.setProperty("--pc-bg", theme.colors.background);
    preview.style.setProperty("--pc-surface", theme.colors.surface);
    preview.style.setProperty("--pc-stroke", theme.colors.stroke);
    preview.style.setProperty("--pc-text", theme.colors.text);
    preview.style.setProperty("--pc-accent", theme.colors.accent);
    preview.style.setProperty("--pc-accent2", theme.colors.accent2);
    preview.style.setProperty("--pc-opacity", `${theme.opacity}%`);

    const chrome = el("div", "preset-card__chrome");
    const mark = el("span", "preset-card__mark");
    const line = el("span", "preset-card__line");
    const clock = el("span", "preset-card__clock");
    chrome.append(mark, line, clock);

    const dots = el("div", "preset-card__dots");
    for (const colour of [
      theme.colors.background,
      theme.colors.surface,
      theme.colors.accent,
      theme.colors.accent2,
    ]) {
      const dot = el("span", "preset-card__dot");
      dot.style.background = colour;
      dots.append(dot);
    }

    preview.append(chrome, dots);

    const body = el("div", "preset-card__copy");
    const head = el("div", "preset-card__head");
    head.append(el("h3", "preset-card__name", theme.name));
    if (active) {
      head.append(el("span", "pill pill--on", t("applied")));
    }

    const description =
      (theme.builtin ? presetText[theme.id]?.[ctx.i18n.language] : undefined) ||
      theme.description ||
      (theme.builtin ? t("presetDescription") : t("customDescription"));
    body.append(head, el("p", "preset-card__desc", description));

    const foot = el("div", "preset-card__foot");
    const apply = button({
      label: busy ? "…" : active ? t("applied") : t("apply"),
      variant: active ? "ghost" : "secondary",
      size: "sm",
      icon: active ? "check" : undefined,
      title: theme.name,
      onClick: () => void applyTheme(theme),
    });
    apply.disabled = active || busy || applyingId !== null;
    apply.dataset.action = "apply-theme";
    apply.dataset.focus = `theme-apply-${theme.id}`;
    if (busy) {
      apply.classList.add("is-busy");
    }

    foot.append(apply);
    card.append(preview, body, foot);
    return card;
  };

  const renderGallery = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("appearance.presets"),
      hint: t("galleryHint"),
      icon: "palette",
      className: "appearance__presets",
    });

    const builtins = themes.filter((theme) => theme.builtin);
    const custom = themes.filter((theme) => !theme.builtin);
    const gallery = el("div", "preset-gallery");
    gallery.setAttribute("role", "list");
    for (const theme of builtins.length > 0 ? builtins : custom) {
      gallery.append(themeCard(theme));
    }

    card.body.append(gallery);

    const customThemes = builtins.length > 0 ? custom : [];
    if (customThemes.length > 0) {
      const more = el("div", "preset-gallery preset-gallery--custom");
      more.setAttribute("role", "list");
      for (const theme of customThemes) {
        more.append(themeCard(theme));
      }

      card.body.append(
        el("div", "panel__subhead", t("customTitle")),
        el("p", "preset-card__sectionHint", t("customHint")),
        more,
      );
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

    card.body.append(nameField);
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
      hex.setAttribute("aria-describedby", `colour-${entry.field}-error`);

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
      error.id = `colour-${entry.field}-error`;
      error.setAttribute("role", "alert");
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
    const label = el("label", "field__label", options.label);
    label.htmlFor = options.id;
    head.append(label);

    const output = el("output", "range-field__value");
    output.htmlFor = options.id;
    output.setAttribute("aria-live", "polite");
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
    const presetFont = fontOptions.some((font) => font.value === draft.font);
    if (!presetFont && draft.font.trim().length > 0) {
      const option = document.createElement("option");
      option.value = draft.font;
      option.textContent = draft.font;
      option.selected = true;
      select.append(option);
    }

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
          description: "",
          theme: { ...cloneTheme(defaultTheme), description: "" },
        });
        if (!themeResult.ok) {
          ctx.notifyFailure(themeResult);
          return false;
        }

        const payload = isRecord(themeResult.data) ? themeResult.data : {};
        const saved = readThemeEntry(payload.theme, defaultTheme) ?? {
          ...cloneTheme(defaultTheme),
          description: "",
          builtin: false,
        };
        activeId = typeof payload.active === "string" ? payload.active : saved.id;
        mergeTheme(saved);
        draftDescription = "";
        ctx.setLocalConfig("theme", cloneTheme(saved));
        ctx.setThemeName(saved.name);

        const savedSections = await ctx.resetSections([
          { section: "animations", values: { enabled: true, speed: 1 } },
        ]);
        if (!savedSections) {
          return false;
        }

        draft = cloneTheme(saved);
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
    left.append(renderGallery(), renderPalette());

    const right = el("div", "appearance__side");
    right.append(renderPreview(), renderShape());

    grid.append(left, right);
    root.replaceChildren(renderHeader(), grid, renderReset());
    setDirtyChrome();
    focus.restore();
  };

  const activate = async (): Promise<void> => {
    loading = true;
    render();

    const result = await ctx.host.call<unknown>("themes.list");
    loading = false;

    if (result.ok) {
      const raw = isRecord(result.data) ? result.data : {};
      themes = readThemes(result.data);

      activeId =
        typeof raw.active === "string" && raw.active.length > 0
          ? raw.active
          : ctx.config().theme.id;

      const configTheme = ctx.config().theme;
      const active =
        themes.find((theme) => theme.id === activeId) ??
        ({
          ...normalizeTheme(configTheme, defaultTheme),
          description: "",
          builtin: false,
        } satisfies ThemeEntry);

      adoptTheme(active);
      ctx.setThemeName(active.name);
    } else {
      // The editor still works with the config copy; only the preset list is missing.
      const fallback: ThemeEntry = {
        ...normalizeTheme(ctx.config().theme, defaultTheme),
        description: "",
        builtin: false,
      };
      themes = themes.length > 0 ? themes : [fallback];
      adoptTheme(themes[0] ?? fallback);
    }

    invalid = new Set();
    render();
  };

  return { el: root, activate, render };
}
