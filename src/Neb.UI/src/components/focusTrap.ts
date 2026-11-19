const Focusable =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Wraps Tab inside a modal card. `<dialog>` already makes the rest of the page inert, but Chrome
 * hands the focus to the browser UI once the last control is passed; this keeps the keyboard in
 * the overlay until it is closed, including when focus somehow starts outside it.
 */
export function trapFocus(container: HTMLElement, event: KeyboardEvent): void {
  if (event.key !== "Tab") {
    return;
  }

  const focusable = Array.from(container.querySelectorAll<HTMLElement>(Focusable)).filter(
    (node) => node.getClientRects().length > 0 && node.tabIndex >= 0,
  );

  if (focusable.length === 0) {
    return;
  }

  const first = focusable[0]!;
  const last = focusable[focusable.length - 1]!;
  const active = document.activeElement;
  const inside = active instanceof HTMLElement && container.contains(active);

  if (!inside) {
    event.preventDefault();
    (event.shiftKey ? last : first).focus();
    return;
  }

  if (event.shiftKey && active === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && active === last) {
    event.preventDefault();
    first.focus();
  }
}
