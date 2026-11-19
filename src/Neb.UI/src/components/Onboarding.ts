import type { I18n, MessageKey } from "../i18n";
import { button, el, switchControl } from "./controls";
import { trapFocus } from "./focusTrap";
import { icon, type IconName } from "./icons";
import { createRestartBadge, type RestartBadge } from "./RestartBadge";

export interface OnboardingModules {
  zapret: boolean;
  discordRp: boolean;
  windhawk: boolean;
}

export interface OnboardingSelection {
  wallpaper: boolean;
  winmode: boolean;
  modules: OnboardingModules;
}

export interface OnboardingOptions {
  /** Read on every open, so the switches mirror the configuration loaded by the handshake. */
  initial(): OnboardingSelection;
  /** Persists the selection together with `onboarded: true`, then reloads the shell. */
  onApply(selection: OnboardingSelection): Promise<boolean>;
  /** Persists only `onboarded: true`; the same path as «Пропустить». */
  onSkip(): Promise<boolean>;
  /** Restarts the shell for module changes that happened during the tour. */
  onRestart(): Promise<void>;
}

export interface Onboarding {
  readonly el: HTMLDialogElement;
  open(): void;
  close(): void;
  render(): void;
  readonly isOpen: boolean;
}

const steps = 3;

interface ModuleRow {
  id: keyof OnboardingModules;
  label: MessageKey;
  desc: MessageKey;
}

const moduleRows: readonly ModuleRow[] = [
  { id: "zapret", label: "onboarding.step3.zapret", desc: "onboarding.step3.zapretDesc" },
  { id: "discordRp", label: "onboarding.step3.discord", desc: "onboarding.step3.discordDesc" },
  { id: "windhawk", label: "onboarding.step3.windhawk", desc: "onboarding.step3.windhawkDesc" },
];

const replacementRows: ReadonlyArray<{ icon: IconName; title: MessageKey; desc: MessageKey }> = [
  { icon: "bar", title: "onboarding.step1.bar", desc: "onboarding.step1.barDesc" },
  { icon: "dock", title: "onboarding.step1.dock", desc: "onboarding.step1.dockDesc" },
  { icon: "launcher", title: "onboarding.step1.launcher", desc: "onboarding.step1.launcherDesc" },
];

/**
 * The three-step first-run tour. It only appears when `config.get` has no `onboarded` flag, and
 * every exit — «Готово», «Пропустить» or Esc — writes the flag, so it can never come back.
 */
export function createOnboarding(i18n: I18n, options: OnboardingOptions): Onboarding {
  const dialog = document.createElement("dialog");
  dialog.className = "modal onboarding";
  dialog.setAttribute("aria-labelledby", "onboarding-title");

  const card = el("div", "onboarding__card");
  const head = el("header", "onboarding__head");
  const stepCopy = el("div", "onboarding__stepCopy");
  const stepLabel = el("span", "onboarding__step");
  stepLabel.id = "onboarding-step";
  const title = el("h2", "onboarding__title");
  title.id = "onboarding-title";
  stepCopy.append(stepLabel, title);

  const skip = el("button", "onboarding__skip");
  skip.type = "button";
  skip.addEventListener("click", () => void skipAll());

  head.append(stepCopy, skip);

  const progress = el("div", "onboarding__progress");
  progress.setAttribute("aria-hidden", "true");

  const body = el("div", "onboarding__body");
  body.id = "onboarding-body";

  const foot = el("footer", "onboarding__foot");
  const back = button({
    label: i18n.t("onboarding.back"),
    variant: "ghost",
    onClick: () => go(step - 1),
  });
  back.id = "onboarding-back";
  const next = button({
    label: i18n.t("onboarding.next"),
    variant: "primary",
    onClick: () => {
      if (step >= steps - 1) {
        void finish();
        return;
      }

      go(step + 1);
    },
  });
  next.id = "onboarding-next";
  foot.append(back, next);

  card.append(head, progress, body, foot);
  dialog.append(card);

  let step = 0;
  let busy = false;
  const initialSelection = (): OnboardingSelection => {
    const value = options.initial();
    return { ...value, modules: { ...value.modules } };
  };
  let selection: OnboardingSelection = initialSelection();
  let changedModules = new Set<keyof OnboardingModules>();
  let restartBadge: RestartBadge | null = null;

  const restart = async (): Promise<void> => {
    await options.onRestart();
    changedModules = new Set();
    restartBadge?.setVisible(false);
  };

  const renderProgress = (): void => {
    progress.replaceChildren();
    for (let index = 0; index < steps; index += 1) {
      const dot = el("span", `onboarding__dot${index === step ? " onboarding__dot--active" : ""}`);
      if (index === step) {
        dot.setAttribute("aria-current", "step");
      }

      progress.append(dot);
    }
  };

  const renderStep = (): void => {
    if (step === 0) {
      title.textContent = i18n.t("onboarding.step1.title");
      const intro = el("p", "onboarding__text", i18n.t("onboarding.step1.text"));
      const list = el("div", "onboarding__replacement");
      for (const row of replacementRows) {
        const item = el("div", "onboarding__replacementRow");
        const glyph = el("span", "onboarding__replacementIcon");
        glyph.innerHTML = icon(row.icon, 18);
        const copy = el("div", "onboarding__replacementCopy");
        copy.append(
          el("span", "onboarding__replacementName", i18n.t(row.title)),
          el("span", "onboarding__replacementDesc", i18n.t(row.desc)),
        );
        item.append(glyph, copy);
        list.append(item);
      }

      body.replaceChildren(intro, list);
      return;
    }

    if (step === 1) {
      title.textContent = i18n.t("onboarding.step2.title");
      const intro = el("p", "onboarding__text", i18n.t("onboarding.step2.text"));

      const list = el("div", "onboarding__settings");
      list.append(
        settingRow(
          "wallpaper",
          "onboarding.step2.wallpaper",
          "onboarding.step2.wallpaperDesc",
          selection.wallpaper,
          (checked) => {
            selection.wallpaper = checked;
            renderStep();
          },
          "wallpapers",
        ),
        settingRow(
          "winmode",
          "onboarding.step2.winmode",
          "onboarding.step2.winmodeDesc",
          selection.winmode,
          (checked) => {
            selection.winmode = checked;
            renderStep();
          },
          "taskbar",
        ),
      );

      body.replaceChildren(intro, list);
      return;
    }

    title.textContent = i18n.t("onboarding.step3.title");
    const intro = el("p", "onboarding__text", i18n.t("onboarding.step3.text"));
    const list = el("div", "onboarding__settings");

    for (const row of moduleRows) {
      list.append(
        settingRow(
          row.id,
          row.label,
          row.desc,
          selection.modules[row.id],
          (checked) => {
            selection.modules[row.id] = checked;
            const baseline = options.initial().modules[row.id];
            if (checked === baseline) {
              changedModules.delete(row.id);
            } else {
              changedModules.add(row.id);
            }

            renderStep();
          },
          "extensions",
        ),
      );
    }

    body.replaceChildren(intro, list);

    if (changedModules.size > 0) {
      restartBadge = createRestartBadge({
        label: i18n.t("restart.pending"),
        action: i18n.t("restart.action"),
        subject: i18n.t("restart.modules"),
        onRestart: restart,
      });
      restartBadge.setVisible(true);
      body.append(restartBadge.el);
    } else {
      restartBadge = null;
    }
  };

  const settingRow = (
    id: string,
    label: MessageKey,
    desc: MessageKey,
    checked: boolean,
    onToggle: (checked: boolean) => void,
    glyph: IconName,
  ): HTMLElement => {
    const row = el("div", "onboarding__row");
    row.dataset.setting = id;
    const mark = el("span", "onboarding__rowIcon");
    mark.innerHTML = icon(glyph, 17);
    const copy = el("div", "onboarding__rowCopy");
    copy.append(
      el("span", "onboarding__rowName", i18n.t(label)),
      el("span", "onboarding__rowDesc", i18n.t(desc)),
    );

    row.append(
      mark,
      copy,
      switchControl({
        checked,
        label: i18n.t(label),
        disabled: busy,
        onToggle,
      }),
    );
    return row;
  };

  const render = (): void => {
    stepLabel.textContent = i18n.t("onboarding.step", { step: step + 1, total: steps });
    skip.textContent = i18n.t("onboarding.skip");
    skip.disabled = busy;
    back.querySelector(".btn__label")!.textContent = i18n.t("onboarding.back");
    back.disabled = busy || step === 0;
    next.querySelector(".btn__label")!.textContent = i18n.t(
      step >= steps - 1 ? "onboarding.done" : "onboarding.next",
    );
    next.disabled = busy;
    renderProgress();
    renderStep();
  };

  const go = (target: number): void => {
    if (busy) {
      return;
    }

    step = Math.max(0, Math.min(steps - 1, target));
    render();
    (step === 0 ? next : step >= steps - 1 ? next : back).focus({ preventScroll: true });
  };

  const close = (): void => {
    if (dialog.open) {
      dialog.close();
    }
  };

  const withBusy = async (action: () => Promise<boolean>): Promise<void> => {
    if (busy) {
      return;
    }

    busy = true;
    render();
    const ok = await action();
    busy = false;
    if (ok) {
      close();
      return;
    }

    render();
  };

  const finish = (): Promise<void> =>
    withBusy(() => options.onApply({ ...selection, modules: { ...selection.modules } }));

  const skipAll = (): Promise<void> => withBusy(() => options.onSkip());

  dialog.addEventListener("keydown", (event) => trapFocus(dialog, event));

  dialog.addEventListener("cancel", (event) => {
    // Esc is a real exit: it persists `onboarded` just like the skip button, so the tour does not
    // return on every launch because someone reached for the keyboard.
    event.preventDefault();
    void skipAll();
  });

  render();

  return {
    el: dialog,
    open() {
      if (dialog.open) {
        return;
      }

      step = 0;
      busy = false;
      selection = initialSelection();
      changedModules = new Set();
      restartBadge = null;
      render();
      dialog.showModal();
      next.focus();
    },
    close,
    render,
    get isOpen() {
      return dialog.open;
    },
  };
}
