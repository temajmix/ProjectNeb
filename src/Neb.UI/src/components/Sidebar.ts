import type { I18n, Language, MessageKey } from "../i18n";
import { Languages } from "../i18n";
import { en } from "../i18n/en";
import { ru } from "../i18n/ru";
import { screenOrder, screenShortcut, type ScreenId } from "../state";
import { icon, type IconName } from "./icons";

/**
 * Nav labels for screens added after the i18n slice froze its tables. `ru`/`en` are the live table
 * objects `createI18n` reads, so adding the two keys here makes the sidebar, the window title and
 * the shortcut sheet agree without editing `i18n/*` (owned by another slice).
 */
const extraNav: Record<string, { ru: string; en: string }> = {
  "nav.profiles": { ru: "Профили", en: "Profiles" },
};

for (const [key, labels] of Object.entries(extraNav)) {
  (ru as unknown as Record<string, string>)[key] = labels.ru;
  (en as unknown as Record<string, string>)[key] = labels.en;
}

export interface SidebarOptions {
  active: ScreenId;
  onSelect(section: ScreenId): void;
  onLanguage(language: Language): void;
  onStyle(): void;
}

export interface Sidebar {
  readonly el: HTMLElement;
  setActive(section: ScreenId): void;
  /** Updates the style chip's value (the shell theme name) without rebuilding the nav. */
  setStyle(name: string): void;
  render(): void;
}

type Section = {
  id: ScreenId;
  label: MessageKey;
  icon: IconName;
  group: "product" | "service";
};

export const screenMeta: Record<ScreenId, Section> = {
  overview: { id: "overview", label: "nav.overview", icon: "home", group: "product" },
  appearance: { id: "appearance", label: "nav.appearance", icon: "palette", group: "product" },
  profiles: {
    id: "profiles",
    label: "nav.profiles" as MessageKey,
    icon: "profiles",
    group: "product",
  },
  zapret: { id: "zapret", label: "nav.zapret", icon: "network", group: "product" },
  discord: { id: "discord", label: "nav.discord", icon: "discord", group: "product" },
  winmode: { id: "winmode", label: "nav.winmode", icon: "taskbar", group: "product" },
  windhawk: { id: "windhawk", label: "nav.windhawk", icon: "extensions", group: "product" },
  bar: { id: "bar", label: "nav.bar", icon: "bar", group: "product" },
  wallpapers: { id: "wallpapers", label: "nav.wallpapers", icon: "wallpapers", group: "product" },
  updates: { id: "updates", label: "nav.updates", icon: "download", group: "service" },
  account: { id: "account", label: "nav.account", icon: "profiles", group: "service" },
  logs: { id: "logs", label: "nav.logs", icon: "terminal", group: "service" },
  about: { id: "about", label: "nav.about", icon: "info", group: "service" },
};

/** Section id -> navigation key, kept for the host's `ui.navigate` messages. */
export const sectionLabels: Record<string, MessageKey> = Object.fromEntries(
  screenOrder.map((id) => [id, screenMeta[id].label]),
);

/** Old C# dashboard ids still arrive from hosts of the previous generation. */
export function resolveScreen(value: string): ScreenId | null {
  if ((screenOrder as readonly string[]).includes(value)) {
    return value as ScreenId;
  }

  const aliases: Record<string, ScreenId> = {
    home: "overview",
    customize: "appearance",
    settings: "appearance",
    network: "zapret",
    zapret: "zapret",
    discord: "discord",
    windows: "winmode",
    winmode: "winmode",
    replacement: "winmode",
    extensions: "windhawk",
    windhawk: "windhawk",
    mods: "windhawk",
    profiles: "account",
  };

  return aliases[value] ?? null;
}

/**
 * The left navigation: a product group, a service group, then the theme chip and the language
 * switch. Below 900 px it becomes an icon rail; every row keeps its label as an accessible name.
 */
export function createSidebar(i18n: I18n, options: SidebarOptions): Sidebar {
  const el = document.createElement("aside");
  el.className = "sidebar";

  let active: ScreenId = options.active;
  let themeName = i18n.t("sidebar.styleValue");

  const render = (): void => {
    const scroll = document.createElement("div");
    scroll.className = "sidebar__scroll";

    const nav = document.createElement("nav");
    nav.className = "nav";
    nav.setAttribute("aria-label", i18n.t("nav.label"));

    const groups: Array<"product" | "service"> = ["product", "service"];
    for (const group of groups) {
      const label = document.createElement("span");
      label.className = "nav__group";
      label.textContent = i18n.t(group === "product" ? "nav.group.product" : "nav.group.service");
      nav.append(label);

      for (const id of screenOrder) {
        const section = screenMeta[id];
        if (section.group !== group) {
          continue;
        }

        const index = screenOrder.indexOf(id) + 1;
        const chord = screenShortcut(index, screenOrder.length);
        const shortcut = chord ? i18n.t("nav.shortcut", { n: chord }) : null;
        const item = document.createElement("button");
        item.type = "button";
        item.className = "nav-item";
        item.dataset.section = id;
        item.title = shortcut ? `${i18n.t(section.label)} · ${shortcut}` : i18n.t(section.label);
        if (chord) {
          item.setAttribute("aria-keyshortcuts", `Control+${chord}`);
        }

        if (id === active) {
          item.setAttribute("aria-current", "page");
        }

        const glyph = document.createElement("span");
        glyph.className = "nav-item__icon";
        glyph.innerHTML = icon(section.icon, 18);

        const text = document.createElement("span");
        text.className = "nav-item__label";
        text.textContent = i18n.t(section.label);

        item.append(glyph, text);
        if (chord) {
          // Eleven sections no longer fit one digit row: the screen without a free chord has no badge.
          const key = document.createElement("span");
          key.className = "nav-item__key";
          key.textContent = chord;
          item.append(key);
        }

        item.addEventListener("click", () => options.onSelect(id));
        nav.append(item);
      }
    }

    scroll.append(nav);

    const foot = document.createElement("div");
    foot.className = "sidebar__foot";

    const languages = document.createElement("div");
    languages.className = "lang-switch";
    languages.setAttribute("role", "group");
    languages.setAttribute("aria-label", i18n.t("sidebar.language"));

    for (const language of Languages) {
      const choice = document.createElement("button");
      choice.type = "button";
      choice.className = "lang-switch__btn";
      choice.dataset.lang = language;
      choice.textContent = language.toUpperCase();
      choice.setAttribute("aria-pressed", String(i18n.language === language));
      choice.addEventListener("click", () => options.onLanguage(language));
      languages.append(choice);
    }

    const style = document.createElement("button");
    style.type = "button";
    style.className = "style-chip";
    style.title = i18n.t("sidebar.styleOpen");
    style.setAttribute("aria-label", i18n.t("sidebar.styleOpen"));

    const sphere = document.createElement("span");
    sphere.className = "style-chip__sphere";

    const text = document.createElement("span");
    text.className = "style-chip__text";
    const label = document.createElement("span");
    label.className = "style-chip__label";
    label.textContent = i18n.t("sidebar.styleLabel");
    const value = document.createElement("span");
    value.className = "style-chip__value";
    value.textContent = themeName;
    text.append(label, value);

    const chevron = document.createElement("span");
    chevron.className = "style-chip__chevron";
    chevron.innerHTML = icon("chevronRight", 16);

    style.append(sphere, text, chevron);
    style.addEventListener("click", options.onStyle);

    foot.append(languages, style);
    el.replaceChildren(scroll, foot);
  };

  render();

  return {
    el,
    setActive(section) {
      if (active === section) {
        return;
      }

      active = section;
      for (const item of el.querySelectorAll<HTMLElement>(".nav-item")) {
        const isActive = item.dataset.section === section;
        item.toggleAttribute("aria-current", isActive);
        if (isActive) {
          item.setAttribute("aria-current", "page");
        }
      }
    },
    setStyle(name) {
      if (name.length === 0 || name === themeName) {
        return;
      }

      themeName = name;
      const value = el.querySelector(".style-chip__value");
      if (value) {
        value.textContent = name;
      }
    },
    render,
  };
}
