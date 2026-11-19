import type { AppContext, Screen } from "../context";
import type { UpdateCheck, ReleaseNotes } from "../components/updateResult";
import { readNotes } from "../components/updateResult";
import {
  button,
  el,
  formatTime,
  panel,
  segmented,
  setBusy,
  skeleton,
  switchControl,
} from "../components/controls";
import { icon } from "../components/icons";
import { showToast } from "../components/toast";

interface LastCheck {
  at: string;
  verdict: UpdateCheck;
}

export function createUpdatesScreen(ctx: AppContext): Screen {
  const root = document.createElement("section");
  root.className = "screen updates-screen";
  root.dataset.screen = "updates";

  let checking = false;
  let last: LastCheck | null = null;
  let failedCheck = false;
  let notes: ReleaseNotes | null = null;
  let notesLoading = true;
  let channel: "stable" | "beta" = ctx.config().updates.channel;
  let autoUpdate = ctx.config().updates.autoUpdate;
  let busy = false;

  const loadNotes = async (): Promise<void> => {
    notesLoading = true;
    const result = await ctx.host.call<unknown>("updates.whatsNew", { channel: ctx.channel() });
    notesLoading = false;

    if (result.ok) {
      notes = readNotes(result.data);
    } else {
      notes = { notes: [], version: null };
    }

    render();
  };

  const check = async (node: HTMLButtonElement): Promise<void> => {
    if (checking) {
      return;
    }

    checking = true;
    setBusy(node, true, ctx.i18n.t("updates.checking"));
    const resultArea = root.querySelector<HTMLElement>("#update-result");
    if (resultArea) {
      resultArea.replaceChildren(skeleton(62));
    }

    const verdict = await ctx.checkUpdates();
    checking = false;

    if (!verdict) {
      failedCheck = true;
      render();
      return;
    }

    failedCheck = false;
    last = { at: new Date().toISOString(), verdict };
    render();

    if (verdict.available) {
      showToast(ctx.i18n.t("updates.result.available", { version: verdict.version ?? "" }), "success");
    } else {
      showToast(ctx.i18n.t("updates.result.none"), "success");
    }
  };

  const openChangelog = (): void => {
    void (async () => {
      const result = await ctx.host.call<unknown>("app.openChangelog");
      if (!result.ok) {
        ctx.notifyFailure(result);
      }
    })();
  };

  const setChannel = async (next: "stable" | "beta"): Promise<void> => {
    if (busy || next === channel) {
      return;
    }

    busy = true;
    const previous = channel;
    channel = next;
    render();

    const saved = await ctx.saveConfig("updates.channel", next);
    busy = false;

    if (saved) {
      showToast(ctx.i18n.t("updates.channelSaved"), "success");
    } else {
      channel = previous;
      render();
    }
  };

  const setAuto = async (next: boolean): Promise<void> => {
    if (busy) {
      return;
    }

    busy = true;
    const previous = autoUpdate;
    autoUpdate = next;
    render();

    const saved = await ctx.saveConfig("updates.autoUpdate", next);
    busy = false;

    if (saved) {
      showToast(ctx.i18n.t("updates.autoSaved"), "success");
    } else {
      autoUpdate = previous;
      render();
    }
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", ctx.i18n.t("updates.title")),
      el("p", "screen__subtitle", ctx.i18n.t("updates.subtitle")),
    );

    const aside = el("div", "screen__aside");
    aside.append(el("span", "pill pill--accent", `v${ctx.version()}`));
    head.append(copy, aside);
    return head;
  };

  const renderResult = (): HTMLElement => {
    const area = el("div", "update-result");
    area.id = "update-result";

    if (checking) {
      area.append(skeleton(62));
      return area;
    }

    if (failedCheck) {
      area.classList.add("update-result--failed");
      const glyph = el("span", "update-result__icon");
      glyph.innerHTML = icon("info", 18);
      const copy = el("div", "update-result__copy");
      copy.append(
        el("span", "update-result__title", ctx.i18n.t("updates.result.failed")),
        el("span", "update-result__hint", ctx.i18n.t("updates.result.failedHint")),
      );
      area.append(glyph, copy);
      return area;
    }

    if (!last) {
      area.classList.add("update-result--empty");
      area.append(
        el("span", "update-result__title", ctx.i18n.t("updates.lastNever")),
        el("span", "update-result__hint", ctx.i18n.t("updates.lastNeverHint")),
      );
      return area;
    }

    const available = last.verdict.available;
    area.classList.add(available ? "update-result--available" : "update-result--ok");

    const glyph = el("span", "update-result__icon");
    glyph.innerHTML = icon(available ? "download" : "check", 18);

    const copy = el("div", "update-result__copy");
    copy.append(
      el(
        "span",
        "update-result__title",
        available
          ? ctx.i18n.t("updates.result.available", { version: last.verdict.version ?? "" })
          : ctx.i18n.t("updates.result.none"),
      ),
    );

    if (available) {
      copy.append(el("span", "update-result__hint", ctx.i18n.t("updates.result.availableHint")));
    }

    copy.append(
      el("span", "update-result__time", ctx.i18n.t("updates.checkedAt", { time: formatTime(last.at, ctx.i18n.language) })),
    );

    area.append(glyph, copy);
    return area;
  };

  const renderCheckPanel = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("updates.checkTitle"),
      icon: "download",
      className: "updates-screen__check",
    });

    const hero = el("div", "version-hero");
    hero.append(
      el("span", "version-hero__label", ctx.i18n.t("updates.current")),
      el("span", "version-hero__value", `v${ctx.version()}`),
      el("span", "version-hero__channel", ctx.i18n.t(channel === "stable" ? "updates.stable" : "updates.beta")),
    );

    const buttonRow = el("div", "updates-screen__actions");
    const checkButton = button({
      label: ctx.i18n.t("updates.check"),
      variant: "primary",
      icon: "download",
      onClick: (event) => void check(event.currentTarget as HTMLButtonElement),
    });
    checkButton.id = "updates-check";
    buttonRow.append(checkButton);

    card.body.append(hero, renderResult(), buttonRow);
    return card.el;
  };

  const renderChannelPanel = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("updates.channel"),
      hint: ctx.i18n.t("updates.channelHint"),
      icon: "tag",
      className: "updates-screen__channel",
    });

    const channelRow = el("div", "field-row field-row--stack");
    channelRow.append(
      segmented<"stable" | "beta">({
        value: channel,
        label: ctx.i18n.t("updates.channel"),
        disabled: busy,
        items: [
          { value: "stable", label: ctx.i18n.t("updates.stable") },
          { value: "beta", label: ctx.i18n.t("updates.beta") },
        ],
        onChange: (value) => void setChannel(value),
      }),
    );

    const autoRow = el("div", "settings__row");
    const glyph = el("span", "settings__icon");
    glyph.innerHTML = icon("refresh", 17);
    const copy = el("span", "settings__text");
    copy.append(
      el("span", "settings__name", ctx.i18n.t("updates.auto")),
      el("span", "settings__desc", ctx.i18n.t("updates.autoHint")),
    );
    autoRow.append(
      glyph,
      copy,
      switchControl({
        checked: autoUpdate,
        label: ctx.i18n.t("updates.auto"),
        disabled: busy,
        onToggle: (next) => void setAuto(next),
      }),
    );

    card.body.append(channelRow, autoRow);
    return card.el;
  };

  const renderNotesPanel = (): HTMLElement => {
    const card = panel({
      title: ctx.i18n.t("updates.notes.title"),
      icon: "sparkles",
      className: "updates-screen__notes",
      actions: [
        button({
          label: ctx.i18n.t("updates.open"),
          variant: "ghost",
          size: "sm",
          iconRight: "external",
          onClick: openChangelog,
        }),
      ],
    });

    if (notesLoading) {
      const wrap = el("div", "notes");
      for (let index = 0; index < 4; index += 1) {
        wrap.append(skeleton(12, index === 3 ? "skeleton--short" : ""));
      }

      card.body.append(wrap);
      return card.el;
    }

    const version = notes?.version;
    if (version) {
      card.body.append(el("span", "panel__subhead", `v${version}`));
    }

    if (!notes || notes.notes.length === 0) {
      card.body.append(el("p", "panel__note", ctx.i18n.t("updates.notes.empty")));
      return card.el;
    }

    const list = el("ul", "notes");
    for (const line of notes.notes) {
      const item = el("li", "notes__item");
      const dot = el("span", "notes__dot");
      item.append(dot, el("span", "notes__text", line));
      list.append(item);
    }

    card.body.append(list);
    return card.el;
  };

  const render = (): void => {
    const grid = el("div", "updates-screen__grid");
    const main = el("div", "updates-screen__main");
    main.append(renderCheckPanel(), renderNotesPanel());
    grid.append(main, renderChannelPanel());
    root.replaceChildren(renderHeader(), grid);
  };

  const activate = async (): Promise<void> => {
    channel = ctx.config().updates.channel;
    autoUpdate = ctx.config().updates.autoUpdate;
    render();
    await loadNotes();
  };

  render();

  return { el: root, activate, render };
}
