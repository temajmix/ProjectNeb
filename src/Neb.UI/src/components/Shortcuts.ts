import type { I18n, MessageKey } from "../i18n";
import { trapFocus } from "./focusTrap";

export interface ShortcutsDialog {
  readonly el: HTMLDialogElement;
  open(): void;
  close(): void;
  render(): void;
  readonly isOpen: boolean;
}

interface ShortcutRow {
  keys: string[];
  label: MessageKey;
}

interface ShortcutGroup {
  title: MessageKey;
  rows: readonly ShortcutRow[];
}

const groups: readonly ShortcutGroup[] = [
  {
    title: "shortcuts.group.nav",
    rows: [
      { keys: ["Ctrl", "1…9"], label: "shortcuts.screens" },
      { keys: ["Ctrl", "0"], label: "shortcuts.last" },
    ],
  },
  {
    title: "shortcuts.group.app",
    rows: [
      { keys: ["Ctrl", "K"], label: "shortcuts.palette" },
      { keys: ["Alt", "Space"], label: "shortcuts.launcher" },
      { keys: ["F1", "?"], label: "shortcuts.help" },
      { keys: ["Ctrl", "M"], label: "shortcuts.minimize" },
      { keys: ["F11"], label: "shortcuts.maximize" },
      { keys: ["Ctrl", "W"], label: "shortcuts.closeWindow" },
      { keys: ["Esc"], label: "shortcuts.closeOverlay" },
    ],
  },
  {
    title: "shortcuts.group.wm",
    rows: [
      { keys: ["Alt", "←↑↓→"], label: "shortcuts.arrows" },
      { keys: ["Alt", "1…9"], label: "shortcuts.workspaces" },
      { keys: ["Alt", "F"], label: "shortcuts.float" },
      { keys: ["Alt", "T"], label: "shortcuts.tile" },
    ],
  },
];

/**
 * The compact F1/`?` reference. It only names shortcuts that exist elsewhere in the product;
 * nothing here is a button, so the list cannot promise an action it does not have.
 */
export function createShortcuts(i18n: I18n): ShortcutsDialog {
  const dialog = document.createElement("dialog");
  dialog.className = "modal shortcuts";
  dialog.setAttribute("aria-labelledby", "shortcuts-title");

  const card = document.createElement("div");
  card.className = "shortcuts__card";

  const head = document.createElement("header");
  head.className = "shortcuts__head";
  const copy = document.createElement("div");
  copy.className = "shortcuts__copy";
  const title = document.createElement("h2");
  title.className = "shortcuts__title";
  title.id = "shortcuts-title";
  const subtitle = document.createElement("p");
  subtitle.className = "shortcuts__subtitle";
  copy.append(title, subtitle);

  const close = document.createElement("button");
  close.type = "button";
  close.className = "icon-btn shortcuts__close";
  close.setAttribute("aria-label", i18n.t("common.close"));
  close.title = i18n.t("common.close");
  close.textContent = "✕";
  close.addEventListener("click", () => dialog.close());

  head.append(copy, close);

  const body = document.createElement("div");
  body.className = "shortcuts__body";

  let trigger: HTMLElement | null = null;

  const foot = document.createElement("footer");
  foot.className = "shortcuts__foot";
  const hint = document.createElement("span");
  hint.className = "shortcuts__hint";
  foot.append(hint);

  card.append(head, body, foot);
  dialog.append(card);

  dialog.addEventListener("keydown", (event) => trapFocus(dialog, event));

  dialog.addEventListener("cancel", (event) => {
    event.preventDefault();
    dialog.close();
  });

  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) {
      dialog.close();
    }
  });

  dialog.addEventListener("close", () => {
    if (trigger && trigger.isConnected) {
      trigger.focus({ preventScroll: true });
    }

    trigger = null;
  });

  const render = (): void => {
    title.textContent = i18n.t("shortcuts.title");
    subtitle.textContent = i18n.t("shortcuts.subtitle");
    hint.textContent = i18n.t("shortcuts.hint");
    close.setAttribute("aria-label", i18n.t("common.close"));
    close.title = i18n.t("common.close");

    body.replaceChildren();
    for (const group of groups) {
      const section = document.createElement("section");
      section.className = "shortcuts__group";
      const heading = document.createElement("h3");
      heading.className = "shortcuts__groupTitle";
      heading.textContent = i18n.t(group.title);
      section.append(heading);

      const list = document.createElement("dl");
      list.className = "shortcuts__list";
      for (const row of group.rows) {
        const item = document.createElement("div");
        item.className = "shortcuts__row";
        const label = document.createElement("dt");
        label.className = "shortcuts__label";
        label.textContent = i18n.t(row.label);
        const keys = document.createElement("dd");
        keys.className = "shortcuts__keys";
        for (const key of row.keys) {
          const chord = document.createElement("kbd");
          chord.className = "shortcuts__key";
          chord.textContent = key;
          keys.append(chord);
        }

        item.append(label, keys);
        list.append(item);
      }

      section.append(list);
      body.append(section);
    }
  };

  render();

  return {
    el: dialog,
    open() {
      if (dialog.open) {
        return;
      }

      trigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      render();
      dialog.showModal();
      close.focus();
    },
    close() {
      if (dialog.open) {
        dialog.close();
      }
    },
    render,
    get isOpen() {
      return dialog.open;
    },
  };
}
