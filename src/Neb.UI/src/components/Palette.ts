import type { I18n } from "../i18n";
import type { ScreenId } from "../state";
import { el } from "./controls";
import { trapFocus } from "./focusTrap";
import { icon } from "./icons";

export interface PaletteTarget {
  screen: ScreenId;
  /** A CSS selector to focus once the screen has rendered, e.g. `#theme-bar-height`. */
  selector?: string;
}

export interface PaletteEntry {
  id: string;
  group: "screens" | "settings";
  label: string;
  hint: string;
  value?: string;
  chord?: string;
  keywords?: string[];
  target: PaletteTarget;
}

export interface PaletteOptions {
  /** Read on every open so values never go stale after a save. */
  entries(): PaletteEntry[];
  onSelect(target: PaletteTarget): void;
}

export interface Palette {
  readonly el: HTMLDialogElement;
  open(): void;
  close(): void;
  readonly isOpen: boolean;
  render(): void;
}

/**
 * «Командная палитра»: one fuzzy list over the sections and the settings that matter most.
 * It is a native `<dialog>`, so the focus trap and Esc come from the platform; the list itself is
 * operated entirely from the search field (`aria-activedescendant`), which keeps the keyboard in
 * one place while the selection moves.
 */
export function createPalette(i18n: I18n, options: PaletteOptions): Palette {
  const dialog = document.createElement("dialog");
  dialog.className = "modal palette";
  dialog.setAttribute("aria-label", i18n.t("palette.title"));

  const card = document.createElement("div");
  card.className = "palette__card";

  const search = document.createElement("div");
  search.className = "palette__search";
  const glyph = document.createElement("span");
  glyph.className = "palette__searchIcon";
  glyph.innerHTML = icon("launcher", 16);

  const input = document.createElement("input");
  input.type = "text";
  input.className = "palette__input";
  input.id = "palette-input";
  input.autocomplete = "off";
  input.spellcheck = false;
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-expanded", "true");
  input.setAttribute("aria-controls", "palette-list");
  input.setAttribute("aria-autocomplete", "list");
  search.append(glyph, input);

  const list = document.createElement("div");
  list.className = "palette__list";
  list.id = "palette-list";
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", i18n.t("palette.title"));

  const empty = document.createElement("p");
  empty.className = "palette__empty";
  empty.hidden = true;

  const foot = document.createElement("footer");
  foot.className = "palette__foot";
  const hint = document.createElement("span");
  hint.className = "palette__hint";
  hint.textContent = i18n.t("palette.hint");
  foot.append(hint);

  card.append(search, list, empty, foot);
  dialog.append(card);

  let entries: PaletteEntry[] = [];
  let shown: PaletteEntry[] = [];
  let selected = 0;
  let trigger: HTMLElement | null = null;

  const label = (entry: PaletteEntry): string => entry.label;

  const score = (query: string, entry: PaletteEntry): number | null => {
    const haystacks = [entry.label, entry.hint, entry.value ?? "", ...(entry.keywords ?? [])]
      .filter((part) => part.length > 0)
      .map((part) => part.toLocaleLowerCase());
    const needle = query.toLocaleLowerCase();

    let best: number | null = null;
    for (const text of haystacks) {
      const direct = text.indexOf(needle);
      if (direct >= 0) {
        const candidate = 40 - direct + (direct === 0 ? 12 : 0);
        best = best === null ? candidate : Math.max(best, candidate);
        continue;
      }

      let position = 0;
      let previous = -2;
      let candidate = 0;
      let matched = true;
      for (const character of needle) {
        const index = text.indexOf(character, position);
        if (index < 0) {
          matched = false;
          break;
        }

        candidate += index === previous + 1 ? 4 : 1;
        if (index === 0 || /[\s·:/(—–-]/.test(text[index - 1] ?? "")) {
          candidate += 3;
        }

        previous = index;
        position = index + 1;
      }

      if (matched) {
        candidate -= Math.max(0, text.length - needle.length) * 0.02;
        best = best === null ? candidate : Math.max(best, candidate);
      }
    }

    return best;
  };

  const filter = (): void => {
    const query = input.value.trim();
    if (query.length === 0) {
      shown = [...entries];
    } else {
      const scored: Array<{ entry: PaletteEntry; score: number; order: number }> = [];
      entries.forEach((entry, order) => {
        const result = score(query, entry);
        if (result !== null) {
          scored.push({ entry, score: result, order });
        }
      });

      scored.sort((left, right) => {
        if (left.entry.group !== right.entry.group) {
          return left.entry.group === "screens" ? -1 : 1;
        }

        return right.score - left.score || left.order - right.order;
      });
      shown = scored.map((item) => item.entry);
    }

    selected = Math.min(selected, Math.max(0, shown.length - 1));
    renderList();
  };

  const choose = (entry: PaletteEntry): void => {
    close();
    options.onSelect(entry.target);
  };

  const renderList = (): void => {
    list.replaceChildren();
    empty.hidden = shown.length > 0;

    let group: PaletteEntry["group"] | null = null;
    shown.forEach((entry, index) => {
      if (entry.group !== group) {
        group = entry.group;
        const heading = document.createElement("div");
        heading.className = "palette__group";
        heading.setAttribute("role", "presentation");
        heading.textContent = i18n.t(
          entry.group === "screens" ? "palette.group.screens" : "palette.group.settings",
        );
        list.append(heading);
      }

      const item = document.createElement("div");
      item.className = `palette__item${index === selected ? " palette__item--active" : ""}`;
      item.id = `palette-item-${index}`;
      item.dataset.entry = entry.id;
      item.setAttribute("role", "option");
      item.setAttribute("aria-selected", String(index === selected));

      const copy = el("span", "palette__copy");
      copy.append(
        el("span", "palette__label", label(entry)),
        el("span", "palette__hintText", entry.hint),
      );

      item.append(copy);

      if (entry.value) {
        const value = document.createElement("span");
        value.className = "palette__value";
        value.textContent = entry.value;
        item.append(value);
      }

      if (entry.chord) {
        const chord = document.createElement("span");
        chord.className = "palette__chord";
        chord.textContent = entry.chord;
        item.append(chord);
      }

      item.addEventListener("mousemove", () => {
        if (selected !== index) {
          selected = index;
          renderList();
        }
      });

      item.addEventListener("click", () => choose(entry));
      list.append(item);
    });

    input.setAttribute("aria-activedescendant", shown.length > 0 ? `palette-item-${selected}` : "");
    if (shown.length === 0) {
      input.removeAttribute("aria-activedescendant");
    }

    scrollSelected();
  };

  const scrollSelected = (): void => {
    const active = list.querySelector<HTMLElement>(`.palette__item--active`);
    active?.scrollIntoView({ block: "nearest" });
  };

  const move = (delta: number): void => {
    if (shown.length === 0) {
      return;
    }

    selected = (selected + delta + shown.length) % shown.length;
    renderList();
  };

  input.addEventListener("input", () => {
    selected = 0;
    filter();
  });

  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      move(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      move(-1);
    } else if (event.key === "Home") {
      event.preventDefault();
      selected = 0;
      renderList();
    } else if (event.key === "End") {
      event.preventDefault();
      selected = Math.max(0, shown.length - 1);
      renderList();
    } else if (event.key === "Enter") {
      event.preventDefault();
      const entry = shown[selected];
      if (entry) {
        choose(entry);
      }
    }
  });

  dialog.addEventListener("keydown", (event) => trapFocus(dialog, event));

  dialog.addEventListener("cancel", (event) => {
    event.preventDefault();
    close();
  });

  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) {
      close();
    }
  });

  const open = (): void => {
    if (dialog.open) {
      return;
    }

    trigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    render();
    dialog.showModal();
    input.focus();
  };

  const close = (): void => {
    if (dialog.open) {
      dialog.close();
    }
  };

  dialog.addEventListener("close", () => {
    if (trigger && trigger.isConnected) {
      trigger.focus({ preventScroll: true });
    }

    trigger = null;
  });

  const render = (): void => {
    dialog.setAttribute("aria-label", i18n.t("palette.title"));
    input.placeholder = i18n.t("palette.placeholder");
    input.setAttribute("aria-label", i18n.t("palette.placeholder"));
    hint.textContent = i18n.t("palette.hint");
    empty.textContent = i18n.t("palette.empty");
    entries = options.entries();
    selected = 0;
    filter();
  };

  return {
    el: dialog,
    open,
    close,
    get isOpen() {
      return dialog.open;
    },
    render,
  };
}
