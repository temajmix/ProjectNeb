import type { I18n } from "../i18n";
import { icon, type IconName } from "./icons";

export interface TitleBarOptions {
  onMinimize(): void;
  onClose(): void;
  onStartDrag(): void;
  onToggleMaximize(): void;
}

export interface TitleBar {
  readonly el: HTMLElement;
  /** Puts the version reported by the host (`app.version`) into the badge. */
  setVersion(version: string | null): void;
  /**
   * Swaps the maximise glyph for `restore` while the host reports a maximised window. Until the
   * host pushes `neb.windowState`, the glyph stays `maximize` — never guessed from `screen.avail*`.
   */
  setMaximized(maximized: boolean): void;
  render(): void;
}

/**
 * The 40 px drag band at the top of the window. WebView2 reports this strip as a non-client caption
 * (see `MainWindow.CaptionBandHeight`), so everything clickable inside it is marked
 * `app-region: no-drag`. The host still owns maximise: a double click on the band toggles it without
 * going through the dashboard.
 */
export function createTitleBar(i18n: I18n, options: TitleBarOptions): TitleBar {
  const el = document.createElement("header");
  el.className = "titlebar";

  let version: string | null = null;
  let maximized = false;

  const button = (
    label: string,
    glyph: IconName,
    action: () => void,
    extraClass = "",
    id = "",
  ): HTMLButtonElement => {
    const element = document.createElement("button");
    element.type = "button";
    element.className = `win-btn${extraClass ? ` ${extraClass}` : ""}`;
    element.title = label;
    element.setAttribute("aria-label", label);
    element.innerHTML = icon(glyph, 14);
    element.addEventListener("click", action);
    if (id) {
      element.id = id;
    }

    return element;
  };

  const render = () => {
    const brand = document.createElement("div");
    brand.className = "brand";

    const mark = document.createElement("span");
    mark.className = "brand__mark";
    mark.setAttribute("role", "img");
    mark.setAttribute("aria-label", i18n.t("titlebar.logo"));

    const name = document.createElement("span");
    name.className = "brand__name";
    name.textContent = i18n.t("app.name");

    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = i18n.t("app.badge", { version: version ?? __APP_VERSION__ });

    brand.append(mark, name, badge);

    const actions = document.createElement("div");
    actions.className = "window-actions";
    actions.append(
      button(i18n.t("titlebar.minimize"), "minimize", options.onMinimize, "", "win-minimize"),
      button(
        i18n.t(maximized ? "titlebar.restore" : "titlebar.maximize"),
        maximized ? "restore" : "maximize",
        options.onToggleMaximize,
        "",
        "win-maximize",
      ),
      button(i18n.t("titlebar.close"), "close", options.onClose, "win-btn--close", "win-close"),
    );

    const focused = document.activeElement instanceof HTMLElement ? document.activeElement.id : "";
    el.replaceChildren(brand, actions);
    if (focused) {
      el.querySelector<HTMLElement>(`#${focused}`)?.focus({ preventScroll: true });
    }
  };

  render();

  // The window is frameless, so the page drives the native move loop and the double-click.
  el.addEventListener("mousedown", (event) => {
    if (event.button !== 0 || (event.target as HTMLElement).closest("button")) {
      return;
    }
    options.onStartDrag();
  });

  el.addEventListener("dblclick", (event) => {
    if (!(event.target as HTMLElement).closest("button")) {
      options.onToggleMaximize();
    }
  });

  return {
    el,
    setVersion(next) {
      if (next && next !== version) {
        version = next;
        render();
      }
    },
    setMaximized(next) {
      if (next === maximized) {
        return;
      }

      maximized = next;
      render();
    },
    render,
  };
}
