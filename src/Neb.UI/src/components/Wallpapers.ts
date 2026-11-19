import type { I18n, MessageKey } from "../i18n";
import { icon } from "./icons";
import { createRestartBadge } from "./RestartBadge";
import { showToast } from "./toast";

/** The wallpaper settings as the host sends them (`WallpaperSettings` over the bridge, camelCase). */
export interface WallpaperSettings {
  enabled: boolean;
  kind: SceneKind;
  path: string | null;
  color: string;
  playlist: string[];
  rotateMinutes: number;
  pauseOnFullscreen: boolean;
  pauseOnBattery: boolean;
  /** Whether the wallpaper process is up right now; read-only, the host owns it. */
  running: boolean;
}

export type SceneKind = "Color" | "Image" | "Video" | "Web";

export interface WallpapersSection {
  readonly el: HTMLElement;
  /** Loads the current state from the host and draws it. Called every time the section is opened. */
  activate(): Promise<void>;
  render(): void;
}

/** `invoke` from `main.ts`: a missing host (a plain browser) resolves to `null`. */
type Invoke = (method: string, params?: unknown) => Promise<unknown>;

const kinds: readonly SceneKind[] = ["Color", "Image", "Video", "Web"];

const kindLabels: Record<SceneKind, MessageKey> = {
  Color: "wallpapers.kind.Color",
  Image: "wallpapers.kind.Image",
  Video: "wallpapers.kind.Video",
  Web: "wallpapers.kind.Web",
};

/**
 * The scene kind a chosen file implies. The host applies the same rule when it validates, so picking a clip
 * switches the section to Video without the user choosing twice.
 */
function kindForPath(path: string): SceneKind | null {
  const extension = path.slice(path.lastIndexOf(".")).toLowerCase();
  if ([".mp4", ".webm", ".mov", ".m4v", ".mkv", ".avi"].includes(extension)) {
    return "Video";
  }

  if ([".png", ".jpg", ".jpeg", ".webp", ".bmp", ".gif", ".avif"].includes(extension)) {
    return "Image";
  }

  if ([".html", ".htm"].includes(extension)) {
    return "Web";
  }

  return null;
}

function fileName(path: string | null): string {
  if (!path) {
    return "";
  }

  const separator = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return separator >= 0 ? path.slice(separator + 1) : path;
}

/**
 * The «Живые обои» section. It owns no state the host does not have: loading reads `wallpaper.getSettings`,
 * every control sends one field to `wallpaper.setSettings` and replaces the local copy with the host's answer,
 * «Выбрать» goes through the host's file dialog, and the buttons start or restart the process. A failed call
 * leaves the previous state on screen and says so.
 */
export function createWallpapersSection(i18n: I18n, invoke: Invoke): WallpapersSection {
  const el = document.createElement("section");
  el.className = "wallpapers";
  el.dataset.section = "wallpapers";

  let settings: WallpaperSettings | null = null;
  let loading = false;
  let busy = false;
  let error: string | null = null;
  /** The values the process was started with, so a reverted change clears the restart pill. */
  let sessionBaseline: WallpaperSettings | null = null;
  /** True once a saved scene value differs from the process that is drawing the desktop. */
  let pendingRestart = false;

  type Attempt<T> =
    | { ok: true; data: T }
    | { ok: false; message: string };

  const attempt = async <T>(method: string, params?: unknown): Promise<Attempt<T>> => {
    try {
      return { ok: true, data: (await invoke(method, params)) as T };
    } catch (failure) {
      return {
        ok: false,
        message: failure instanceof Error ? failure.message : String(failure),
      };
    }
  };

  const render = (): void => {
    if (loading) {
      el.replaceChildren(message(i18n.t("wallpapers.loading"), "settings__state"));
      return;
    }

    if (!settings) {
      el.replaceChildren(
        message(error ?? i18n.t("wallpapers.unavailable.text"), "settings__state"),
      );
      return;
    }

    el.replaceChildren(createSceneCard(), createSideCard());
    el.classList.toggle("settings--busy", busy);
  };

  const createSceneCard = (): HTMLElement => {
    const card = document.createElement("article");
    card.className = "card settings__card wallpapers__card";

    const head = document.createElement("header");
    head.className = "settings__head";

    const title = document.createElement("h2");
    title.className = "card__title";
    title.textContent = i18n.t("wallpapers.scene.title");

    head.append(title, createSwitch({
      id: "wallpaper-enabled",
      checked: settings?.enabled === true,
      label: i18n.t("wallpapers.enabled.title"),
      onToggle: (next) => void patch({ enabled: next }),
    }));

    const hint = document.createElement("p");
    hint.className = "settings__hint";
    hint.textContent = i18n.t("wallpapers.scene.hint");

    const restartBadge = createRestartBadge({
      label: i18n.t("restart.pending"),
      action: i18n.t("restart.action"),
      subject: i18n.t("restart.wallpapers"),
      onRestart: restartWallpapers,
    });
    restartBadge.setVisible(pendingRestart);

    const rows = document.createElement("div");
    rows.className = "settings__rows";
    rows.append(
      createEnabledRow(),
      createKindRow(),
      createFileRow(),
      createColourRow(),
      createRotateRow(),
    );

    const pauseTitle = document.createElement("h3");
    pauseTitle.className = "wallpapers__subtitle";
    pauseTitle.textContent = i18n.t("wallpapers.pause.title");

    const pauseRows = document.createElement("div");
    pauseRows.className = "settings__rows";
    pauseRows.append(
      createPauseRow("fullscreen", "wallpapers.pause.fullscreen", "wallpapers.pause.fullscreen.desc", settings?.pauseOnFullscreen === true),
      createPauseRow("battery", "wallpapers.pause.battery", "wallpapers.pause.battery.desc", settings?.pauseOnBattery === true),
    );

    card.append(head, hint, restartBadge.el, rows, pauseTitle, pauseRows);
    return card;
  };

  const createEnabledRow = (): HTMLElement => {
    const row = document.createElement("div");
    row.className = "settings__row";

    const glyph = document.createElement("span");
    glyph.className = "settings__icon";
    glyph.innerHTML = icon("wallpapers", 18);

    const text = document.createElement("span");
    text.className = "settings__text";
    const name = document.createElement("span");
    name.className = "settings__name";
    name.textContent = i18n.t("wallpapers.enabled.title");
    const desc = document.createElement("span");
    desc.className = "settings__desc";
    desc.textContent = i18n.t("wallpapers.enabled.desc");
    text.append(name, desc);

    row.append(glyph, text, createSwitch({
      id: "wallpaper-enabled-row",
      checked: settings?.enabled === true,
      label: i18n.t("wallpapers.enabled.title"),
      onToggle: (next) => void patch({ enabled: next }),
    }));

    return row;
  };

  const createKindRow = (): HTMLElement => {
    const row = document.createElement("div");
    row.className = "settings__row settings__row--stack";

    const text = document.createElement("span");
    text.className = "settings__text";
    const name = document.createElement("span");
    name.className = "settings__name";
    name.textContent = i18n.t("wallpapers.kind.label");
    text.append(name);

    const group = document.createElement("div");
    group.className = "segmented";
    group.setAttribute("role", "radiogroup");
    group.setAttribute("aria-label", i18n.t("wallpapers.kind.label"));

    for (const kind of kinds) {
      const choice = document.createElement("button");
      choice.type = "button";
      choice.className = `segmented__btn${settings?.kind === kind ? " segmented__btn--active" : ""}`;
      choice.dataset.kind = kind;
      choice.setAttribute("role", "radio");
      choice.setAttribute("aria-checked", String(settings?.kind === kind));
      choice.textContent = i18n.t(kindLabels[kind]);
      choice.disabled = busy;

      choice.addEventListener("click", () => {
        if (settings?.kind !== kind) {
          void patch({ kind });
        }
      });

      group.append(choice);
    }

    row.append(text, group);
    return row;
  };

  const createFileRow = (): HTMLElement => {
    const row = document.createElement("div");
    row.className = "settings__row";

    const glyph = document.createElement("span");
    glyph.className = "settings__icon";
    glyph.innerHTML = icon("folder", 18);

    const text = document.createElement("span");
    text.className = "settings__text";
    const name = document.createElement("span");
    name.className = "settings__name";
    name.textContent = i18n.t("wallpapers.file.title");
    const desc = document.createElement("span");
    desc.className = "settings__desc wallpapers__path";
    desc.textContent = settings?.path ? fileName(settings.path) : i18n.t("wallpapers.file.none");
    desc.title = settings?.path ?? "";
    text.append(name, desc);

    const choose = document.createElement("button");
    choose.type = "button";
    choose.id = "wallpaper-choose";
    choose.className = "wallpapers__choose";
    choose.textContent = i18n.t("wallpapers.file.choose");
    choose.disabled = busy;
    choose.addEventListener("click", () => void chooseFile());

    row.append(glyph, text, choose);
    return row;
  };

  const createColourRow = (): HTMLElement => {
    const row = document.createElement("div");
    row.className = "settings__row";

    const glyph = document.createElement("span");
    glyph.className = "settings__icon";
    glyph.innerHTML = icon("palette", 18);

    const text = document.createElement("span");
    text.className = "settings__text";
    const name = document.createElement("span");
    name.className = "settings__name";
    name.textContent = i18n.t("wallpapers.color.title");
    const desc = document.createElement("span");
    desc.className = "settings__desc";
    desc.textContent = i18n.t("wallpapers.color.hint");
    text.append(name, desc);

    const input = document.createElement("input");
    input.type = "color";
    input.id = "wallpaper-colour";
    input.className = "colour-input";
    input.value = settings?.color ?? "#141021";
    input.disabled = busy;
    input.setAttribute("aria-label", i18n.t("wallpapers.color.title"));
    input.addEventListener("change", () => void patch({ color: input.value.toUpperCase() }));

    row.append(glyph, text, input);
    return row;
  };

  const createRotateRow = (): HTMLElement => {
    const row = document.createElement("div");
    row.className = "settings__row";

    const glyph = document.createElement("span");
    glyph.className = "settings__icon";
    glyph.innerHTML = icon("clock", 18);

    const count = settings?.playlist.length ?? 0;
    const text = document.createElement("span");
    text.className = "settings__text";
    const name = document.createElement("span");
    name.className = "settings__name";
    name.textContent = `${i18n.t("wallpapers.rotate.title")}, ${i18n.t("wallpapers.rotate.unit")}`;
    const desc = document.createElement("span");
    desc.className = "settings__desc";
    desc.textContent = i18n.t("wallpapers.rotate.hint", {
      count: count === 0 ? i18n.t("wallpapers.playlist.empty") : i18n.t("wallpapers.playlist.count", { count }),
    });
    text.append(name, desc);

    const actions = document.createElement("span");
    actions.className = "wallpapers__inline";

    const input = document.createElement("input");
    input.type = "number";
    input.id = "wallpaper-rotate";
    input.className = "number-input";
    input.min = "1";
    input.max = "1440";
    input.step = "1";
    input.value = String(settings?.rotateMinutes ?? 30);
    input.disabled = busy;
    input.setAttribute("aria-label", i18n.t("wallpapers.rotate.title"));
    input.addEventListener("change", () => void patch({ rotateMinutes: Number(input.value) }));

    actions.append(input);

    if (count > 0) {
      const clear = document.createElement("button");
      clear.type = "button";
      clear.id = "wallpaper-clear-playlist";
      clear.className = "wallpapers__choose wallpapers__choose--quiet";
      clear.textContent = i18n.t("wallpapers.playlist.clear");
      clear.disabled = busy;
      clear.addEventListener("click", () => void patch({ playlist: [] }));
      actions.append(clear);
    }

    row.append(glyph, text, actions);
    return row;
  };

  const createPauseRow = (id: string, title: MessageKey, desc: MessageKey, checked: boolean): HTMLElement => {
    const row = document.createElement("div");
    row.className = "settings__row";

    const glyph = document.createElement("span");
    glyph.className = "settings__icon";
    glyph.innerHTML = icon("pause", 18);

    const text = document.createElement("span");
    text.className = "settings__text";
    const name = document.createElement("span");
    name.className = "settings__name";
    name.textContent = i18n.t(title);
    const hint = document.createElement("span");
    hint.className = "settings__desc";
    hint.textContent = i18n.t(desc);
    text.append(name, hint);

    row.append(glyph, text, createSwitch({
      id: `wallpaper-pause-${id}`,
      checked,
      label: i18n.t(title),
      onToggle: (next) => void patch(id === "fullscreen" ? { pauseOnFullscreen: next } : { pauseOnBattery: next }),
    }));

    return row;
  };

  const createSideCard = (): HTMLElement => {
    const card = document.createElement("article");
    card.className = "card settings__card wallpapers__side";

    const head = document.createElement("header");
    head.className = "settings__head";
    const title = document.createElement("h2");
    title.className = "card__title";
    title.textContent = i18n.t("wallpapers.preview.title");

    const running = settings?.running === true;
    const status = document.createElement("span");
    status.className = `settings__status${running ? " settings__status--on" : ""}`;
    status.id = "wallpaper-status";
    status.textContent = i18n.t(running ? "wallpapers.status.running" : "wallpapers.status.stopped");
    head.append(title, status);

    // A still card, not the live scene: the real thing is on the desktop, and a second renderer in the
    // dashboard would cost exactly what pausing exists to avoid.
    const preview = document.createElement("div");
    preview.className = "preview";
    preview.style.background =
      `radial-gradient(circle at 28% 22%, ${settings?.color ?? "#141021"} 0 42%, rgba(11, 10, 18, 0.98) 100%)`;

    const art = document.createElement("div");
    art.className = "preview__art";
    preview.append(art);

    const kind = settings?.kind === "Color" ? "wallpapers.preview.color" : "wallpapers.preview.file";
    const caption = document.createElement("span");
    caption.className = "preview__caption";
    caption.textContent =
      settings?.kind === "Color"
        ? `${i18n.t(kind)} · ${settings?.color ?? ""}`
        : `${i18n.t(kind)} · ${settings?.kind === "Web" && !settings.path ? "aurora" : fileName(settings?.path ?? "") || "—"}`;
    preview.append(caption);

    const hint = document.createElement("p");
    hint.className = "settings__hint";
    hint.textContent = i18n.t("wallpapers.preview.hint");

    const actions = document.createElement("div");
    actions.className = "settings__actions";

    if (!running) {
      const start = document.createElement("button");
      start.type = "button";
      start.id = "wallpaper-start";
      start.className = "settings__restart";
      start.textContent = i18n.t("wallpapers.start");
      start.disabled = busy;
      start.addEventListener("click", () => void startWallpapers());
      actions.append(start);
    }

    const restart = document.createElement("button");
    restart.type = "button";
    restart.id = "wallpaper-restart";
    restart.className = "settings__restart";
    restart.textContent = i18n.t("wallpapers.restart");
    restart.disabled = busy;
    restart.addEventListener("click", () => void restartWallpapers());
    actions.append(restart);

    const note = document.createElement("span");
    note.className = "settings__note";
    note.textContent = i18n.t("wallpapers.restart.hint");

    card.append(head, preview, hint, actions, note);
    return card;
  };

  const createSwitch = (options: {
    id: string;
    checked: boolean;
    label: string;
    onToggle: (next: boolean) => void;
  }): HTMLButtonElement => {
    const button = document.createElement("button");
    button.type = "button";
    button.id = options.id;
    button.className = `switch${options.checked ? " switch--on" : ""}`;
    button.setAttribute("role", "switch");
    button.setAttribute("aria-checked", String(options.checked));
    button.setAttribute("aria-label", options.label);
    button.disabled = busy;

    const thumb = document.createElement("span");
    thumb.className = "switch__thumb";
    button.append(thumb);

    button.addEventListener("click", () => options.onToggle(!options.checked));
    return button;
  };

  const activate = async (): Promise<void> => {
    loading = true;
    error = null;
    busy = false;
    render();

    const current = await attempt<WallpaperSettings | null>("wallpaper.getSettings");
    if (!current.ok || !current.data) {
      error = current.ok ? i18n.t("wallpapers.unavailable.text") : current.message;
      settings = null;
    } else {
      settings = current.data;
      sessionBaseline ??= JSON.parse(JSON.stringify(current.data)) as WallpaperSettings;
    }

    loading = false;
    render();
  };

  /** Sends one partial settings patch and adopts the host's answer, or reverts to what is on disk. */
  const patch = async (params: Record<string, unknown>): Promise<void> => {
    if (busy) {
      return;
    }

    busy = true;
    render();

    const result = await attempt<WallpaperSettings | null>("wallpaper.setSettings", params);
    busy = false;

    if (result.ok && result.data) {
      settings = result.data;
      pendingRestart =
        sessionBaseline === null || JSON.stringify(result.data) !== JSON.stringify(sessionBaseline);
      showToast(i18n.t("wallpapers.saved"), "success");
      render();
      return;
    }

    showToast(result.ok ? i18n.t("wallpapers.failed") : result.message, "error");
    await activate();
  };

  /**
   * The host's own file dialog, then the patch. The kind follows the extension unless the user picked a page
   * for a Web scene; the host applies the same rule, so the two can never disagree for long.
   */
  const chooseFile = async (): Promise<void> => {
    if (busy) {
      return;
    }

    busy = true;
    render();

    const picked = await attempt<{ path?: string | null } | null>("wallpaper.pickFile");
    busy = false;

    if (!picked.ok) {
      showToast(picked.message, "error");
      render();
      return;
    }

    const path = picked.data?.path ?? null;
    if (!path) {
      render();
      return;
    }

    const inferred = kindForPath(path);
    await patch({ path, ...(inferred ? { kind: inferred } : {}) });
  };

  const startWallpapers = async (): Promise<void> => {
    if (busy) {
      return;
    }

    busy = true;
    render();

    const result = await attempt<WallpaperSettings | null>("wallpaper.setSettings", {
      enabled: true,
    });
    busy = false;

    if (result.ok && result.data) {
      settings = result.data;
      showToast(i18n.t("wallpapers.start.done"), "success");
      render();
      return;
    }

    showToast(result.ok ? i18n.t("wallpapers.failed") : result.message, "error");
    await activate();
  };

  const restartWallpapers = async (): Promise<void> => {
    if (busy) {
      return;
    }

    busy = true;
    render();

    const result = await attempt<{ ok?: boolean; running?: boolean } | null>("wallpaper.restart");
    busy = false;

    if (result.ok && result.data?.ok !== false) {
      pendingRestart = false;
      showToast(i18n.t("wallpapers.restart.done"), "success");
      await activate();
      sessionBaseline = settings
        ? (JSON.parse(JSON.stringify(settings)) as WallpaperSettings)
        : null;
      return;
    }

    showToast(result.ok ? i18n.t("wallpapers.failed") : result.message, "error");
    await activate();
  };

  render();

  return { el, activate, render };
}

function message(text: string, className: string): HTMLElement {
  const element = document.createElement("p");
  element.className = className;
  element.textContent = text;
  return element;
}
