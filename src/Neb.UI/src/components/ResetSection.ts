import { button, el } from "./controls";
import { confirmDialog } from "./Modal";

export interface ResetSectionOptions {
  /** «Сбросить к настройкам по умолчанию». */
  label: string;
  /** The button text; the full label is the card title, so the button can stay short. */
  action?: string;
  hint: string;
  confirmTitle: string;
  confirmText: string;
  confirmAction: string;
  cancel: string;
  id?: string;
  /** Resolves true when the section was written; the caller owns success/failure toasts. */
  onReset(): Promise<boolean>;
}

/**
 * The footer control a settings screen uses to return its own section to the shipped values.
 * The confirmation is the shared native dialog, so Esc and the focus trap behave exactly like
 * every other destructive action in the window.
 */
export function resetSectionCard(options: ResetSectionOptions): HTMLElement {
  const card = el("section", "section-reset");
  const copy = el("div", "section-reset__copy");
  copy.append(
    el("h2", "section-reset__title", options.label),
    el("p", "section-reset__hint", options.hint),
  );

  let busy = false;
  const reset = button({
    label: options.action ?? options.label,
    variant: "ghost",
    size: "sm",
    icon: "refresh",
    onClick: () => {
      if (busy) {
        return;
      }

      void (async () => {
        const confirmed = await confirmDialog({
          title: options.confirmTitle,
          text: options.confirmText,
          confirm: options.confirmAction,
          cancel: options.cancel,
          danger: true,
        });

        if (!confirmed) {
          return;
        }

        busy = true;
        reset.disabled = true;
        reset.classList.add("is-busy");
        try {
          await options.onReset();
        } finally {
          busy = false;
          reset.disabled = false;
          reset.classList.remove("is-busy");
        }
      })();
    },
  });

  if (options.id) {
    reset.id = options.id;
  }

  card.append(copy, reset);
  return card;
}
