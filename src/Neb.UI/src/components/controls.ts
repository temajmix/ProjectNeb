import type { MessageKey } from "../i18n";
import type { I18n } from "../i18n";
import { icon, type IconName } from "./icons";

/** Small DOM helpers shared by every screen. */

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) {
    node.className = className;
  }

  if (text !== undefined) {
    node.textContent = text;
  }

  return node;
}

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

export interface ButtonOptions {
  variant?: ButtonVariant;
  size?: "sm" | "md";
  icon?: IconName;
  iconRight?: IconName;
  label: string;
  /** A native tooltip, useful when the label alone is not the whole story. */
  title?: string;
  id?: string;
  onClick?: (event: MouseEvent) => void;
}

export function button(options: ButtonOptions): HTMLButtonElement {
  const node = el("button", "btn");
  node.type = "button";
  node.classList.add(`btn--${options.variant ?? "secondary"}`);
  if (options.size === "sm") {
    node.classList.add("btn--sm");
  }

  if (options.id) {
    node.id = options.id;
  }

  if (options.title) {
    node.title = options.title;
  }

  if (options.icon) {
    const glyph = el("span", "btn__icon");
    glyph.innerHTML = icon(options.icon, options.size === "sm" ? 15 : 16);
    node.append(glyph);
  }

  node.append(el("span", "btn__label", options.label));

  if (options.iconRight) {
    const glyph = el("span", "btn__icon btn__icon--right");
    glyph.innerHTML = icon(options.iconRight, 15);
    node.append(glyph);
  }

  if (options.onClick) {
    node.addEventListener("click", options.onClick);
  }

  return node;
}

export function iconButton(
  glyph: IconName,
  label: string,
  onClick: (event: MouseEvent) => void,
  className = "",
): HTMLButtonElement {
  const node = el("button", `icon-btn${className ? ` ${className}` : ""}`);
  node.type = "button";
  node.title = label;
  node.setAttribute("aria-label", label);
  node.innerHTML = icon(glyph, 16);
  node.addEventListener("click", onClick);
  return node;
}

export function setBusy(node: HTMLButtonElement, busy: boolean, label: string): void {
  node.disabled = busy;
  node.classList.toggle("is-busy", busy);
  node.setAttribute("aria-busy", String(busy));

  const old = node.querySelector(".spinner");
  if (busy && !old) {
    const spinner = el("span", "spinner");
    spinner.setAttribute("aria-hidden", "true");
    node.prepend(spinner);
  } else if (!busy && old) {
    old.remove();
  }

  const text = node.querySelector(".btn__label");
  if (text) {
    text.textContent = label;
  }
}

export interface SwitchOptions {
  checked: boolean;
  label: string;
  onToggle(next: boolean): void;
  disabled?: boolean;
  id?: string;
  title?: string;
}

/** A `role="switch"` button, styled from tokens, with the thumb the settings rows use. */
export function switchControl(options: SwitchOptions): HTMLButtonElement {
  const node = el("button", `switch${options.checked ? " switch--on" : ""}`);
  node.type = "button";
  node.setAttribute("role", "switch");
  node.setAttribute("aria-checked", String(options.checked));
  node.setAttribute("aria-label", options.label);
  node.disabled = options.disabled === true;

  if (options.id) {
    node.id = options.id;
  }

  if (options.title) {
    node.title = options.title;
  }

  node.append(el("span", "switch__thumb"));
  node.addEventListener("click", () => options.onToggle(!options.checked));
  return node;
}

export interface SegmentedOption<T extends string> {
  value: T;
  label: string;
}

export function segmented<T extends string>(options: {
  value: T;
  items: readonly SegmentedOption<T>[];
  label: string;
  disabled?: boolean;
  onChange(value: T): void;
}): HTMLElement {
  const group = el("div", "segmented");
  group.setAttribute("role", "radiogroup");
  group.setAttribute("aria-label", options.label);

  for (const item of options.items) {
    const choice = el("button", "segmented__btn", item.label);
    choice.type = "button";
    choice.setAttribute("role", "radio");
    choice.setAttribute("aria-checked", String(item.value === options.value));
    choice.classList.toggle("segmented__btn--active", item.value === options.value);
    choice.disabled = options.disabled === true;
    choice.addEventListener("click", () => {
      if (item.value !== options.value) {
        options.onChange(item.value);
      }
    });
    group.append(choice);
  }

  return group;
}

export function pill(text: string, tone: "neutral" | "on" | "accent" | "danger" | "warn" = "neutral"): HTMLElement {
  return el("span", `pill pill--${tone}`, text);
}

/** A shimmering stand-in with the final layout's height. */
export function skeleton(height: number, className = ""): HTMLElement {
  const node = el("div", `skeleton${className ? ` ${className}` : ""}`);
  node.style.height = `${height}px`;
  node.setAttribute("aria-hidden", "true");
  return node;
}

export function skeletonPanel(lines: number, title = true): HTMLElement {
  const card = el("article", "card skeleton-card");
  if (title) {
    card.append(skeleton(14, "skeleton--title"));
  }

  for (let index = 0; index < lines; index += 1) {
    card.append(skeleton(12, index === lines - 1 ? "skeleton--short" : ""));
  }

  return card;
}

export function emptyState(options: {
  icon: IconName;
  title: string;
  text: string;
  action?: HTMLButtonElement;
}): HTMLElement {
  const node = el("div", "empty");
  const glyph = el("span", "empty__icon");
  glyph.innerHTML = icon(options.icon, 22);
  node.append(glyph, el("h3", "empty__title", options.title), el("p", "empty__text", options.text));
  if (options.action) {
    node.append(options.action);
  }

  return node;
}

export function panel(options: {
  title?: string;
  hint?: string;
  icon?: IconName;
  actions?: HTMLElement[];
  className?: string;
}): { el: HTMLElement; body: HTMLElement; head: HTMLElement | null } {
  const card = el("section", `card panel${options.className ? ` ${options.className}` : ""}`);
  const body = el("div", "panel__body");

  if (options.title) {
    const head = el("header", "panel__head");
    const titleWrap = el("div", "panel__titleWrap");
    if (options.icon) {
      const glyph = el("span", "panel__icon");
      glyph.innerHTML = icon(options.icon, 17);
      titleWrap.append(glyph);
    }

    titleWrap.append(el("h2", "panel__title", options.title));
    head.append(titleWrap);

    if (options.actions && options.actions.length > 0) {
      const actions = el("div", "panel__actions");
      actions.append(...options.actions);
      head.append(actions);
    }

    card.append(head);
    if (options.hint) {
      card.append(el("p", "panel__hint", options.hint));
    }

    card.append(body);
    return { el: card, body, head };
  }

  card.append(body);
  return { el: card, body, head: null };
}

/** `<dd>`-style value line used by the overview, updates and about screens. */
export function metaRow(label: string, value: string, mono = false): HTMLElement {
  const row = el("div", "meta-row");
  row.append(el("span", "meta-row__label", label));
  row.append(el("span", `meta-row__value${mono ? " meta-row__value--mono" : ""}`, value));
  return row;
}

export function formatClock(date: Date, format: "24" | "12", language: string): string {
  return new Intl.DateTimeFormat(language === "en" ? "en-US" : "ru-RU", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: format === "12",
  }).format(date);
}

export function formatDate(date: Date, language: string): string {
  return new Intl.DateTimeFormat(language === "en" ? "en-US" : "ru-RU", {
    weekday: "short",
    day: "numeric",
    month: "long",
  }).format(date);
}

export function formatTime(iso: string, language: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) {
    return iso;
  }

  return new Intl.DateTimeFormat(language === "en" ? "en-US" : "ru-RU", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(date);
}

export function tr(i18n: I18n, key: MessageKey, params?: Record<string, string | number>): string {
  return i18n.t(key, params);
}
