export type ToastKind = "info" | "success" | "error";

let toast: HTMLElement | null = null;
let dot: HTMLElement | null = null;
let timer = 0;

/**
 * One toast at a time, bottom centre. `success` confirms a write, `error` says a call did not land
 * (including a method the host build does not have) and `info` is neutral.
 *
 * A toast is a live region: an error is assertive so a failed save is announced even while the
 * focus is on a form control, while a success message waits for a pause.
 */
export function showToast(message: string, kind: ToastKind = "info"): void {
  if (!toast || !toast.isConnected) {
    toast = document.createElement("div");
    toast.className = "toast";

    dot = document.createElement("span");
    dot.className = "toast__dot";
    dot.setAttribute("aria-hidden", "true");

    const text = document.createElement("span");
    text.className = "toast__text";

    toast.append(dot, text);
    document.body.append(toast);
  }

  const text = toast.querySelector(".toast__text");
  if (text) {
    text.textContent = message;
  }

  toast.dataset.kind = kind;
  toast.setAttribute("role", kind === "error" ? "alert" : "status");
  toast.setAttribute("aria-live", kind === "error" ? "assertive" : "polite");
  toast.setAttribute("aria-atomic", "true");
  toast.classList.add("toast--visible");

  window.clearTimeout(timer);
  timer = window.setTimeout(() => {
    toast?.classList.remove("toast--visible");
  }, kind === "error" ? 4200 : 2800);
}
