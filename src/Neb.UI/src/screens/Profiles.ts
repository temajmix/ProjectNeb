import type { AppContext, Screen } from "../context";
import {
  button,
  el,
  emptyState,
  panel,
  skeletonPanel,
} from "../components/controls";
import { icon } from "../components/icons";
import { confirmDialog } from "../components/Modal";
import { showToast } from "../components/toast";

/**
 * The Profiles screen owns its copy until `i18n/*` catches up: that module belongs to another
 * slice in this round, so the ru/en sentences live next to the screen they describe.
 */
const copy = {
  ru: {
    title: "Профили",
    subtitle: "Сохранённые сочетания темы и настроек оболочки",
    listTitle: "Ваши профили",
    listHint: "Применение возвращает тему, бар, док и модули одной кнопкой.",
    formTitle: "Сохранить текущий как профиль",
    formHint: "Профиль запоминает активную тему и снимок оболочки.",
    name: "Название профиля",
    namePlaceholder: "Например, «Ночь»",
    save: "Сохранить текущий как профиль",
    saving: "Сохраняю…",
    apply: "Применить",
    applying: "Применяю…",
    delete: "Удалить",
    emptyTitle: "Пока нет профилей",
    emptyText:
      "Профиль хранит тему и настройки оболочки: бар, док, закреплённые приложения и модули. Сохраните текущий вид, чтобы вернуться к нему одной кнопкой.",
    emptyAction: "Создать первый профиль",
    count: "Профилей: {count}",
    current: "Сейчас: {name}",
    theme: "Тема: {id}",
    barOn: "бар",
    barOff: "бар скрыт",
    dockOn: "док",
    dockOff: "док скрыт",
    dockAuto: "автоскрытие дока",
    launcher: "лаунчер",
    taskbarHidden: "панель задач скрыта",
    pinned: "закреплено: {count}",
    barHeight: "высота {height} px",
    saved: "Профиль «{name}» сохранён",
    saveFailed: "Не удалось сохранить профиль",
    applied: "Профиль «{name}» применён",
    applyFailed: "Не удалось применить профиль",
    deleted: "Профиль «{name}» удалён",
    deleteFailed: "Не удалось удалить профиль",
    deleteTitle: "Удалить профиль «{name}»?",
    deleteText: "Файл профиля будет удалён. Тема и настройки оболочки останутся на месте.",
    deleteConfirm: "Удалить",
    cancel: "Отмена",
    nameRequired: "Дайте профилю название",
    nameTooLong: "Название длиннее 80 символов",
    loading: "Читаю профили…",
    unavailable: "Этот хост не умеет профили. Обновите ProjectNeb.",
  },
  en: {
    title: "Profiles",
    subtitle: "Saved combinations of a theme and the shell settings",
    listTitle: "Your profiles",
    listHint: "Applying one restores the theme, bar, dock and modules in one press.",
    formTitle: "Save the current look as a profile",
    formHint: "A profile remembers the active theme and a snapshot of the shell.",
    name: "Profile name",
    namePlaceholder: "For example, “Night”",
    save: "Save the current look as a profile",
    saving: "Saving…",
    apply: "Apply",
    applying: "Applying…",
    delete: "Delete",
    emptyTitle: "No profiles yet",
    emptyText:
      "A profile stores the theme and the shell settings: bar, dock, pinned apps and modules. Save the current look to return to it with one press.",
    emptyAction: "Create the first profile",
    count: "Profiles: {count}",
    current: "Now: {name}",
    theme: "Theme: {id}",
    barOn: "bar",
    barOff: "bar hidden",
    dockOn: "dock",
    dockOff: "dock hidden",
    dockAuto: "dock auto-hide",
    launcher: "launcher",
    taskbarHidden: "taskbar hidden",
    pinned: "pinned: {count}",
    barHeight: "{height} px high",
    saved: "Profile “{name}” saved",
    saveFailed: "The profile could not be saved",
    applied: "Profile “{name}” applied",
    applyFailed: "The profile could not be applied",
    deleted: "Profile “{name}” deleted",
    deleteFailed: "The profile could not be deleted",
    deleteTitle: "Delete the profile “{name}”?",
    deleteText: "The profile file is removed. The theme and shell settings stay as they are.",
    deleteConfirm: "Delete",
    cancel: "Cancel",
    nameRequired: "Give the profile a name",
    nameTooLong: "The name is longer than 80 characters",
    loading: "Reading profiles…",
    unavailable: "This host does not implement profiles. Update ProjectNeb.",
  },
} as const;

type ProfileCopyKey = keyof (typeof copy)["ru"];

interface ShellSnapshot {
  barEnabled: boolean;
  barPosition: string;
  barHeight: number;
  dockEnabled: boolean;
  dockAutoHide: boolean;
  launcherEnabled: boolean;
  hideTaskbar: boolean;
  pinnedApps: string[];
}

interface ProfileEntry {
  id: string;
  name: string;
  themeId: string;
  shell: ShellSnapshot;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function text(value: unknown, fallback: string): string {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : fallback;
}

function integer(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? Math.round(value) : fallback;
}

function readProfile(raw: unknown): ProfileEntry | null {
  if (!isRecord(raw)) {
    return null;
  }

  const id = text(raw.id, "");
  if (id.length === 0) {
    return null;
  }

  const shell = isRecord(raw.shell) ? raw.shell : {};
  return {
    id,
    name: text(raw.name, id),
    themeId: text(raw.themeId ?? raw.theme_id, ""),
    shell: {
      barEnabled: bool(shell.barEnabled ?? shell.bar_enabled, true),
      barPosition: text(shell.barPosition ?? shell.bar_position, "top"),
      barHeight: integer(shell.barHeight ?? shell.bar_height, 40),
      dockEnabled: bool(shell.dockEnabled ?? shell.dock_enabled, true),
      dockAutoHide: bool(shell.dockAutoHide ?? shell.dock_auto_hide, true),
      launcherEnabled: bool(shell.launcherEnabled ?? shell.launcher_enabled, true),
      hideTaskbar: bool(shell.hideTaskbar ?? shell.hide_taskbar, false),
      pinnedApps: Array.isArray(shell.pinnedApps)
        ? shell.pinnedApps.filter((entry): entry is string => typeof entry === "string")
        : [],
    },
  };
}

function readProfiles(raw: unknown): ProfileEntry[] {
  const list = isRecord(raw) && Array.isArray(raw.profiles) ? raw.profiles : [];
  return list
    .map(readProfile)
    .filter((profile): profile is ProfileEntry => profile !== null);
}

export function createProfilesScreen(ctx: AppContext): Screen {
  const root = document.createElement("section");
  root.className = "screen profiles-screen";
  root.dataset.screen = "profiles";

  const t = (key: ProfileCopyKey, params?: Record<string, string | number>): string => {
    const template = copy[ctx.i18n.language][key];
    if (!params) {
      return template;
    }

    return template.replace(/\{(\w+)\}/g, (match, name: string) =>
      params[name] === undefined ? match : String(params[name]),
    );
  };

  let profiles: ProfileEntry[] = [];
  let activeThemeName = ctx.config().theme.name;
  let nameDraft = "";
  let nameError: ProfileCopyKey | null = null;
  let loading = false;
  let busy: { kind: "save" } | { kind: "profile"; id: string } | null = null;

  const load = async (): Promise<void> => {
    loading = true;
    render();

    const [profilesResult, themesResult] = await Promise.all([
      ctx.host.call<unknown>("profiles.list"),
      ctx.host.call<unknown>("themes.list"),
    ]);

    loading = false;
    if (profilesResult.ok) {
      profiles = readProfiles(profilesResult.data);
    }

    if (themesResult.ok && isRecord(themesResult.data)) {
      const active = text(themesResult.data.active, "");
      const list = Array.isArray(themesResult.data.themes)
        ? themesResult.data.themes
        : [];
      const theme = list.find(
        (entry): entry is Record<string, unknown> =>
          isRecord(entry) && entry.id === active,
      );
      if (theme) {
        activeThemeName = text(theme.name, active);
      }
    }

    render();
  };

  const summary = (shell: ShellSnapshot): string => {
    const parts = [
      shell.barEnabled ? t("barOn") : t("barOff"),
      shell.barEnabled ? t("barHeight", { height: shell.barHeight }) : null,
      shell.dockEnabled ? t("dockOn") : t("dockOff"),
      shell.dockEnabled && shell.dockAutoHide ? t("dockAuto") : null,
      shell.launcherEnabled ? t("launcher") : null,
      shell.hideTaskbar ? t("taskbarHidden") : null,
      shell.pinnedApps.length > 0 ? t("pinned", { count: shell.pinnedApps.length }) : null,
    ].filter((part): part is string => part !== null);

    return parts.join(" · ");
  };

  const saveProfile = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    const name = nameDraft.trim();
    if (name.length === 0) {
      nameError = "nameRequired";
      render();
      root.querySelector<HTMLInputElement>("#profile-name")?.focus();
      return;
    }

    if (name.length > 80) {
      nameError = "nameTooLong";
      render();
      root.querySelector<HTMLInputElement>("#profile-name")?.focus();
      return;
    }

    nameError = null;
    busy = { kind: "save" };
    render();

    const result = await ctx.host.call<unknown>("profiles.save", {
      name,
      profile: { name },
    });
    busy = null;

    if (!result.ok) {
      if (result.reason === "unavailable") {
        showToast(t("unavailable"), "error");
      } else {
        showToast(t("saveFailed"), "error");
      }

      render();
      return;
    }

    profiles = readProfiles(result.data);
    nameDraft = "";
    showToast(t("saved", { name }), "success");
    render();
  };

  const applyProfile = async (profile: ProfileEntry): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = { kind: "profile", id: profile.id };
    render();

    const result = await ctx.host.call<unknown>("profiles.apply", {
      id: profile.id,
      profileId: profile.id,
    });
    busy = null;

    if (!result.ok) {
      if (result.reason === "unavailable") {
        showToast(t("unavailable"), "error");
      } else {
        showToast(t("applyFailed"), "error");
      }

      render();
      return;
    }

    const payload = isRecord(result.data) ? result.data : {};
    const config = isRecord(payload.config) ? payload.config : {};
    const shell = isRecord(config.shell) ? config.shell : {};

    const themeId = text(shell.themeId ?? shell.theme_id, profile.themeId);
    const barEnabled = bool(shell.barEnabled ?? shell.bar_enabled, profile.shell.barEnabled);
    const dockEnabled = bool(shell.dockEnabled ?? shell.dock_enabled, profile.shell.dockEnabled);
    const launcherEnabled = bool(
      shell.launcherEnabled ?? shell.launcher_enabled,
      profile.shell.launcherEnabled,
    );

    ctx.setLocalConfig("shell.themeId", themeId);
    ctx.setLocalConfig("shell.barEnabled", barEnabled);
    ctx.setLocalConfig("shell.dockEnabled", dockEnabled);
    ctx.setLocalConfig("shell.launcherEnabled", launcherEnabled);
    ctx.setLocalConfig(
      "shell.hideTaskbar",
      bool(shell.hideTaskbar ?? shell.hide_taskbar, profile.shell.hideTaskbar),
    );
    ctx.setLocalConfig(
      "shell.barHeight",
      integer(shell.barHeight ?? shell.bar_height, profile.shell.barHeight),
    );
    ctx.setLocalConfig(
      "shell.dockAutoHide",
      bool(shell.dockAutoHide ?? shell.dock_auto_hide, profile.shell.dockAutoHide),
    );

    const bar = isRecord(shell.bar) ? shell.bar : {};
    if (Array.isArray(bar.widgets)) {
      ctx.setLocalConfig("shell.widgets", bar.widgets);
    }

    const themeName = await refreshThemeName(themeId);
    if (themeName) {
      activeThemeName = themeName;
      ctx.setLocalConfig("theme.id", themeId);
      ctx.setThemeName(themeName);
    }

    await ctx.refreshShell();
    showToast(t("applied", { name: profile.name }), "success");
    render();
  };

  const refreshThemeName = async (themeId: string): Promise<string> => {
    const result = await ctx.host.call<unknown>("themes.list");
    if (!result.ok || !isRecord(result.data)) {
      return "";
    }

    const list = Array.isArray(result.data.themes) ? result.data.themes : [];
    const theme = list.find(
      (entry): entry is Record<string, unknown> => isRecord(entry) && entry.id === themeId,
    );

    return theme ? text(theme.name, themeId) : themeId;
  };

  const deleteProfile = async (profile: ProfileEntry): Promise<void> => {
    if (busy !== null) {
      return;
    }

    const confirmed = await confirmDialog({
      title: t("deleteTitle", { name: profile.name }),
      text: t("deleteText"),
      confirm: t("deleteConfirm"),
      cancel: t("cancel"),
      danger: true,
    });
    if (!confirmed) {
      return;
    }

    busy = { kind: "profile", id: profile.id };
    render();

    const result = await ctx.host.call<unknown>("profiles.delete", {
      id: profile.id,
      profileId: profile.id,
    });
    busy = null;

    if (!result.ok) {
      if (result.reason === "unavailable") {
        showToast(t("unavailable"), "error");
      } else {
        showToast(t("deleteFailed"), "error");
      }

      render();
      return;
    }

    profiles = readProfiles(result.data);
    showToast(t("deleted", { name: profile.name }), "success");
    render();
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(el("h1", "screen__title", t("title")), el("p", "screen__subtitle", t("subtitle")));

    const aside = el("div", "screen__aside");
    aside.append(el("span", "pill pill--neutral", t("current", { name: activeThemeName })));

    head.append(copy, aside);
    return head;
  };

  const renderRow = (profile: ProfileEntry): HTMLElement => {
    const row = el("article", "profile-row");
    row.dataset.profile = profile.id;

    const glyph = el("span", "profile-row__glyph");
    glyph.innerHTML = icon("profiles", 18);
    glyph.setAttribute("aria-hidden", "true");

    const body = el("div", "profile-row__copy");
    const head = el("div", "profile-row__head");
    head.append(el("h3", "profile-row__name", profile.name));
    head.append(el("span", "pill pill--neutral", t("theme", { id: profile.themeId })));
    body.append(head, el("p", "profile-row__meta", summary(profile.shell)));

    const actions = el("div", "profile-row__actions");
    const applying = busy?.kind === "profile" && busy.id === profile.id;
    const apply = button({
      label: applying ? t("applying") : t("apply"),
      variant: "secondary",
      size: "sm",
      icon: "check",
      onClick: () => void applyProfile(profile),
    });
    apply.disabled = busy !== null;
    apply.dataset.action = "apply-profile";

    const remove = button({
      label: t("delete"),
      variant: "ghost",
      size: "sm",
      icon: "trash",
      onClick: () => void deleteProfile(profile),
    });
    remove.disabled = busy !== null;
    remove.dataset.action = "delete-profile";
    remove.classList.add("profile-row__delete");

    actions.append(apply, remove);
    row.append(glyph, body, actions);
    return row;
  };

  const renderList = (): HTMLElement => {
    const card = panel({
      title: t("listTitle"),
      hint: t("listHint"),
      icon: "profiles",
      className: "profiles-screen__list",
    });

    if (loading) {
      const wrap = el("div", "profile-list");
      wrap.append(skeletonPanel(2), skeletonPanel(2));
      card.body.append(wrap);
      return card.el;
    }

    if (profiles.length === 0) {
      const create = button({
        label: t("emptyAction"),
        variant: "secondary",
        size: "sm",
        icon: "plus",
        onClick: () => root.querySelector<HTMLInputElement>("#profile-name")?.focus(),
      });
      card.body.append(
        emptyState({
          icon: "profiles",
          title: t("emptyTitle"),
          text: t("emptyText"),
          action: create,
        }),
      );
      return card.el;
    }

    const list = el("div", "profile-list");
    for (const profile of profiles) {
      list.append(renderRow(profile));
    }

    card.body.append(list);
    return card.el;
  };

  const renderForm = (): HTMLElement => {
    const card = panel({
      title: t("formTitle"),
      hint: t("formHint"),
      icon: "plus",
      className: "profiles-screen__form",
    });

    const field = el("label", "field profile-name-field");
    field.htmlFor = "profile-name";
    field.append(el("span", "field__label", t("name")));

    const input = document.createElement("input");
    input.className = "input";
    input.id = "profile-name";
    input.type = "text";
    input.value = nameDraft;
    input.maxLength = 80;
    input.spellcheck = false;
    input.autocomplete = "off";
    input.placeholder = t("namePlaceholder");
    input.setAttribute("aria-invalid", String(nameError !== null));
    if (nameError) {
      input.setAttribute("aria-describedby", "profile-name-error");
    }

    input.addEventListener("input", () => {
      nameDraft = input.value;
      if (nameError !== null) {
        nameError = null;
        input.removeAttribute("aria-invalid");
        input.removeAttribute("aria-describedby");
        const error = root.querySelector<HTMLElement>("#profile-name-error");
        if (error) {
          error.hidden = true;
        }
      }
    });

    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void saveProfile();
      }
    });

    field.append(input);

    const error = el("span", "field__error", nameError ? t(nameError) : "");
    error.id = "profile-name-error";
    error.hidden = nameError === null;
    field.append(error);

    const actions = el("div", "profiles-screen__formActions");
    const submit = button({
      label: busy?.kind === "save" ? t("saving") : t("save"),
      variant: "primary",
      size: "sm",
      icon: "plus",
      onClick: () => void saveProfile(),
    });
    submit.disabled = busy !== null;
    submit.dataset.action = "save-profile";
    submit.id = "profile-save";
    actions.append(submit);

    const note = el(
      "p",
      "profiles-screen__formNote",
      t("current", { name: activeThemeName }),
    );

    card.body.append(field, actions, note);
    return card.el;
  };

  const render = (): void => {
    const grid = el("div", "profiles-screen__grid");
    grid.append(renderList(), renderForm());
    root.replaceChildren(renderHeader(), grid);
  };

  const activate = async (): Promise<void> => {
    await load();
  };

  render();

  return { el: root, activate, render };
}
