import { button, el } from "./controls";

let modalSeq = 0;

export interface ConfirmOptions {
  title: string;
  text: string;
  confirm: string;
  cancel: string;
  /** Paints the confirming button red — used for quitting. */
  danger?: boolean;
}

/**
 * A native `<dialog>`: focus trapping, Esc and the inert backdrop come from the platform, so the
 * confirmation the quick actions open can never leave the keyboard outside the modal.
 */
export function confirmDialog(options: ConfirmOptions): Promise<boolean> {
  const dialog = el("dialog", `modal${options.danger ? " modal--danger" : ""}`);
  modalSeq += 1;
  const titleId = `modal-title-${modalSeq}`;
  const textId = `modal-text-${modalSeq}`;

  const card = el("div", "modal__card");
  const title = el("h2", "modal__title", options.title);
  title.id = titleId;
  const text = el("p", "modal__text", options.text);
  text.id = textId;

  const actions = el("div", "modal__actions");

  const close = (result: boolean): void => {
    if (!dialog.open) {
      return;
    }

    dialog.close(result ? "confirm" : "cancel");
  };

  const cancel = button({
    label: options.cancel,
    variant: "ghost",
    onClick: () => close(false),
  });

  const confirm = button({
    label: options.confirm,
    variant: options.danger ? "danger" : "primary",
    onClick: () => close(true),
  });

  actions.append(cancel, confirm);
  card.append(title, text, actions);
  dialog.append(card);
  dialog.setAttribute("aria-labelledby", titleId);
  dialog.setAttribute("aria-describedby", textId);

  return new Promise<boolean>((resolve) => {
    let answered = false;
    const trigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const finish = (value: boolean): void => {
      if (answered) {
        return;
      }

      answered = true;
      resolve(value);
    };

    dialog.addEventListener("cancel", (event) => {
      event.preventDefault();
      close(false);
    });

    dialog.addEventListener("click", (event) => {
      // The backdrop is the dialog element itself; the card stops its own clicks from falling through.
      if (event.target === dialog) {
        close(false);
      }
    });

    dialog.addEventListener("close", () => {
      finish(dialog.returnValue === "confirm");
      dialog.remove();
      // `showModal` normally restores the trigger, but the node may already be gone by the time
      // the async caller re-renders; put the keyboard back explicitly while it is still there.
      if (trigger && trigger.isConnected) {
        trigger.focus({ preventScroll: true });
      }
    });

    document.body.append(dialog);
    dialog.showModal();
    confirm.focus();
  });
}
