/**
 * Arrow-key movement for a list of rows.
 *
 * A screen marks its rows with `data-roving-item` and calls this once per render. Enter and Space are
 * left alone, so a row that is a button still activates the way the browser says it should; the
 * helper only moves focus. Home/End jump to the edges, and the walk wraps around, which is what a
 * short preset or rule list expects.
 */

const RowSelector = "[data-roving-item]";

function isTextEntry(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }

  const tag = target.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || target.isContentEditable;
}

export function attachRovingKeys(
  container: HTMLElement,
  options: { orientation?: "vertical" | "horizontal" } = {},
): void {
  const horizontal = options.orientation === "horizontal";

  container.addEventListener("keydown", (event) => {
    if (isTextEntry(event.target)) {
      return;
    }

    const previous = horizontal ? "ArrowLeft" : "ArrowUp";
    const next = horizontal ? "ArrowRight" : "ArrowDown";
    const handled = event.key === previous || event.key === next || event.key === "Home" || event.key === "End";
    if (!handled) {
      return;
    }

    const rows = Array.from(container.querySelectorAll<HTMLElement>(RowSelector)).filter(
      (row) => !row.hasAttribute("disabled") && !row.hidden && row.getClientRects().length > 0,
    );

    if (rows.length === 0) {
      return;
    }

    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const index = active ? rows.findIndex((row) => row === active || row.contains(active)) : -1;

    let target: HTMLElement | undefined;
    if (event.key === "Home") {
      target = rows[0];
    } else if (event.key === "End") {
      target = rows[rows.length - 1];
    } else if (event.key === previous) {
      target = rows[index <= 0 ? rows.length - 1 : index - 1];
    } else {
      target = rows[index < 0 || index === rows.length - 1 ? 0 : index + 1];
    }

    if (!target) {
      return;
    }

    event.preventDefault();
    target.focus({ preventScroll: false });
    target.scrollIntoView({ block: "nearest" });
  });
}
