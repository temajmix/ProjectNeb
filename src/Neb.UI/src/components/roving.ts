/**
 * Arrow-key movement for lists and radio groups.
 *
 * A screen marks its rows with `data-roving-item` and calls `attachRovingKeys` once per render.
 * Enter and Space are left alone, so a row that is a button still activates the way the browser
 * says it should; the helper only moves focus. Home/End jump to the edges, and the walk wraps
 * around, which is what a short preset or rule list expects. The focused row is the single Tab stop
 * of the group (roving tabindex), so Tab enters the list once instead of visiting every row.
 */

const RowSelector = "[data-roving-item]";

function isTextEntry(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }

  const tag = target.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || target.isContentEditable;
}

function allRows(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(RowSelector)).filter(
    (row) => !row.hasAttribute("disabled") && !row.hidden,
  );
}

function visibleRows(container: HTMLElement): HTMLElement[] {
  return allRows(container).filter((row) => row.getClientRects().length > 0);
}

/** One Tab stop per list; the arrows move it. */
function setRovingTabStop(rows: HTMLElement[], current: HTMLElement | null): void {
  if (rows.length === 0) {
    return;
  }

  const active = current && rows.includes(current) ? current : rows[0];
  for (const row of rows) {
    row.tabIndex = row === active ? 0 : -1;
  }
}

export function attachRovingKeys(
  container: HTMLElement,
  options: { orientation?: "vertical" | "horizontal" } = {},
): void {
  const horizontal = options.orientation === "horizontal";

  const rows = allRows(container);
  const checked = rows.find((row) => row.getAttribute("aria-checked") === "true");
  setRovingTabStop(rows, checked ?? null);

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

    const currentRows = visibleRows(container);
    if (currentRows.length === 0) {
      return;
    }

    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const index = active ? currentRows.findIndex((row) => row === active || row.contains(active)) : -1;

    let target: HTMLElement | undefined;
    if (event.key === "Home") {
      target = currentRows[0];
    } else if (event.key === "End") {
      target = currentRows[currentRows.length - 1];
    } else if (event.key === previous) {
      target = currentRows[index <= 0 ? currentRows.length - 1 : index - 1];
    } else {
      target = currentRows[index < 0 || index === currentRows.length - 1 ? 0 : index + 1];
    }

    if (!target) {
      return;
    }

    event.preventDefault();
    setRovingTabStop(currentRows, target);
    target.focus({ preventScroll: false });
    target.scrollIntoView({ block: "nearest" });
  });
}

/**
 * Arrow keys for a `role="radiogroup"` whose children are `role="radio"` buttons. Selection
 * follows focus, which is the platform behaviour for radios; `click()` runs the control's own
 * handler so the screen keeps a single source of truth.
 */
export function attachRadioKeys(group: HTMLElement): void {
  const allRadios = (): HTMLElement[] =>
    Array.from(group.querySelectorAll<HTMLElement>('[role="radio"]')).filter(
      (radio) => !radio.hasAttribute("disabled"),
    );

  const radios = (): HTMLElement[] => allRadios().filter((radio) => radio.getClientRects().length > 0);

  const sync = (): void => {
    const list = allRadios();
    const checked = list.find((radio) => radio.getAttribute("aria-checked") === "true");
    for (const radio of list) {
      radio.tabIndex = radio === (checked ?? list[0]) ? 0 : -1;
    }
  };

  sync();

  group.addEventListener("click", () => {
    // Selection follows the click; keep the Tab stop on the checked radio. Deferred so a
    // re-rendering parent can replace the group first.
    queueMicrotask(sync);
  });

  group.addEventListener("keydown", (event) => {
    if (isTextEntry(event.target)) {
      return;
    }

    const keys = ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"];
    if (!keys.includes(event.key)) {
      return;
    }

    const list = radios();
    if (list.length === 0) {
      return;
    }

    event.preventDefault();
    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const index = active ? list.findIndex((radio) => radio === active || radio.contains(active)) : -1;

    let target: HTMLElement | undefined;
    if (event.key === "Home") {
      target = list[0];
    } else if (event.key === "End") {
      target = list[list.length - 1];
    } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
      target = list[index <= 0 ? list.length - 1 : index - 1];
    } else {
      target = list[index < 0 || index === list.length - 1 ? 0 : index + 1];
    }

    if (!target) {
      return;
    }

    target.focus({ preventScroll: true });
    target.click();
  });
}
