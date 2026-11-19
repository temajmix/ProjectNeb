/**
 * Focus helpers shared by the screens and the overlays.
 *
 * A screen rebuilds its subtree on every render, which is cheap and keeps the markup declarative,
 * but the browser moves focus to `<body>` the moment the focused node is replaced. Remembering the
 * focused control by a stable id (or `data-focus` key) lets the rebuild put the keyboard back where
 * it was instead of making a keyboard user start from the top of the window.
 */

/** A stable selector for `node`, or an empty string when it has nothing to remember it by. */
export function focusKey(node: Element | null): string {
  if (!(node instanceof HTMLElement)) {
    return "";
  }

  if (node.id.length > 0) {
    return `#${CSS.escape(node.id)}`;
  }

  const key = node.dataset.focus;
  return key ? `[data-focus="${CSS.escape(key)}"]` : "";
}

/** The id of the control that currently owns focus, if it lives inside `root`. */
export function captureFocus(root: HTMLElement): string {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement) || !root.contains(active)) {
    return "";
  }

  return focusKey(active);
}

/** Focuses the remembered control, unless it disappeared or is disabled. */
export function restoreFocus(root: HTMLElement, key: string): boolean {
  if (key.length === 0) {
    return false;
  }

  const node = root.querySelector<HTMLElement>(key);
  if (!node || !node.isConnected || node.hasAttribute("disabled") || node.getClientRects().length === 0) {
    return false;
  }

  if (document.activeElement !== node) {
    node.focus({ preventScroll: true });
  }

  return true;
}

/**
 * Keeps the last focused control of `root` across rebuilds. `restore()` is called at the end of a
 * render; it only acts when the focused element is gone (a fresh, already-focused node is left
 * alone). Async renders are covered too: the last key survives while the call is in flight.
 */
export function watchFocus(root: HTMLElement): { restore(): void } {
  let last = "";

  root.addEventListener("focusin", (event) => {
    const key = focusKey(event.target instanceof Element ? event.target : null);
    if (key.length > 0) {
      last = key;
    }
  });

  return {
    restore() {
      if (last.length === 0) {
        return;
      }

      const active = document.activeElement;
      if (active instanceof HTMLElement && root.contains(active) && active !== document.body) {
        // The screen already moved focus somewhere meaningful (for example to a field it just
        // revealed); a global restore would steal it back.
        const key = focusKey(active);
        if (key.length > 0) {
          last = key;
        }

        return;
      }

      if (restoreFocus(root, last)) {
        return;
      }

      // The remembered control was removed by its own action (a deleted row). Fall back to the
      // first visible control of the screen rather than dropping the keyboard on <body>.
      const fallback = root.querySelector<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex="0"]',
      );
      fallback?.focus({ preventScroll: true });
    },
  };
}
