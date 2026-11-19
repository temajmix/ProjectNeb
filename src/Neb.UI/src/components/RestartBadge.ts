import { button, el } from "./controls";

export interface RestartBadgeOptions {
  /** «Нужен перезапуск» — the pill's text. */
  label: string;
  /** «Перезапустить» — the action's text. */
  action: string;
  /** Optional context, e.g. «Обои» or «Модули». */
  subject?: string;
  onRestart(): void | Promise<void>;
}

export interface RestartBadge {
  readonly el: HTMLElement;
  setVisible(visible: boolean): void;
}

/**
 * A small honest pill for a setting whose saved value only reaches a child process on restart.
 * It is rendered by the screen that knows whether the value changed in this session; this
 * component does not guess. The action is a real host call supplied by the caller.
 */
export function createRestartBadge(options: RestartBadgeOptions): RestartBadge {
  const root = el("div", "restart-badge");
  root.hidden = true;
  root.setAttribute("role", "status");
  root.setAttribute("aria-live", "polite");

  root.append(el("span", "pill pill--warn", options.label));
  if (options.subject) {
    root.append(el("span", "restart-badge__subject", options.subject));
  }

  let busy = false;

  const restart = button({
    label: options.action,
    variant: "ghost",
    size: "sm",
    icon: "refresh",
    onClick: () => {
      if (busy) {
        return;
      }

      busy = true;
      restart.disabled = true;
      void Promise.resolve(options.onRestart()).finally(() => {
        busy = false;
        restart.disabled = false;
      });
    },
  });
  restart.classList.add("restart-badge__action");
  root.append(restart);

  return {
    el: root,
    setVisible(visible) {
      root.hidden = !visible;
    },
  };
}
