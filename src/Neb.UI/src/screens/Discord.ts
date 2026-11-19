import type { AppContext, Screen } from "../context";
import type { CallResult } from "../host";
import type { MessageKey } from "../i18n";
import {
  button,
  el,
  emptyState,
  iconButton,
  panel,
  pill,
  setBusy,
  skeletonPanel,
  switchControl,
} from "../components/controls";
import { confirmDialog } from "../components/Modal";
import { attachRovingKeys } from "../components/roving";
import { showToast } from "../components/toast";
import { watchFocus } from "../components/focus";
import { countLabel, nouns } from "../i18n/plural";

/**
 * «Discord RP» — Rich Presence.
 *
 * Presence is a claim about the user's Discord account, so this screen never invents one: the badge
 * is read from `discord.getState` alone, and a host that does not implement the methods shows
 * «Недоступно в этой сборке». Presets are edited locally and written with `discord.savePreset`;
 * rules are the match list written wholesale with `discord.setRules`.
 */

type RecordValue = Record<string, unknown>;
type ConnectionStatus = "off" | "connected" | "error";
type MatchKind = "exe" | "class" | "title";
type Busy = "connect" | "disconnect" | "save" | "delete" | "activate" | "rules" | "saveAppId" | null;

/**
 * Labels this screen owns; the shared `i18n` tables belong to another slice, so the pairs stay
 * next to the controls that use them.
 */
const copy = {
  ru: {
    saveAppId: "Сохранить app ID",
    savingAppId: "Сохраняю…",
    where: "Где взять ID",
    savedId: "Сохранённый ID",
    saveFailed: "Не удалось сохранить app ID",
  },
  en: {
    saveAppId: "Save app ID",
    savingAppId: "Saving…",
    where: "Where to get an ID",
    savedId: "Saved ID",
    saveFailed: "Could not save the app ID",
  },
} as const;

/** Discord application ids are snowflakes: 17 to 20 digits and nothing else. */
function validAppId(value: string): boolean {
  return /^\d{17,20}$/.test(value.trim());
}

interface PresetButton {
  label: string;
  url: string;
}

interface Preset {
  id: string;
  name: string;
  details: string;
  state: string;
  largeImageKey: string;
  largeImageText: string;
  smallImageKey: string;
  smallImageText: string;
  buttons: PresetButton[];
  startTimestamp: boolean;
}

interface Rule {
  id: string;
  match: MatchKind;
  pattern: string;
  presetId: string;
}

interface DiscordState {
  status: ConnectionStatus;
  appId: string;
  error: string | null;
  presets: Preset[];
  activePresetId: string | null;
}

interface NewRule {
  match: MatchKind;
  pattern: string;
  presetId: string;
}

function isRecord(value: unknown): value is RecordValue {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function pick(source: RecordValue, ...keys: string[]): unknown {
  for (const key of keys) {
    const value = source[key];
    if (value !== undefined && value !== null) {
      return value;
    }
  }

  return undefined;
}

function text(value: unknown): string {
  return typeof value === "string" ? value : typeof value === "number" ? String(value) : "";
}

function flag(value: unknown): boolean | null {
  if (typeof value === "boolean") {
    return value;
  }

  if (typeof value === "string") {
    const lower = value.toLowerCase();
    if (lower === "true" || lower === "yes" || lower === "on") {
      return true;
    }

    if (lower === "false" || lower === "no" || lower === "off") {
      return false;
    }
  }

  return null;
}

function readButtons(raw: unknown): PresetButton[] {
  const list = Array.isArray(raw)
    ? raw
    : isRecord(raw) && Array.isArray(raw.buttons)
      ? raw.buttons
      : [];

  const buttons: PresetButton[] = [];
  for (const entry of list) {
    if (!isRecord(entry)) {
      continue;
    }

    const label = text(pick(entry, "label", "text", "name")).trim();
    const url = text(pick(entry, "url", "link", "href")).trim();
    if (label.length === 0 && url.length === 0) {
      continue;
    }

    buttons.push({ label, url });
    if (buttons.length === 2) {
      break;
    }
  }

  return buttons;
}

function readPreset(raw: unknown): Preset | null {
  if (!isRecord(raw)) {
    return null;
  }

  const name = text(pick(raw, "name", "title", "label")).trim();
  const id = text(pick(raw, "id", "key", "presetId")).trim() || name;
  if (id.length === 0) {
    return null;
  }

  return {
    id,
    name: name || `${id}`,
    details: text(pick(raw, "details", "description", "line1")),
    state: text(pick(raw, "state", "status", "line2")),
    largeImageKey: text(pick(raw, "largeImageKey", "large_image", "largeImage", "imageKey")),
    largeImageText: text(pick(raw, "largeImageText", "large_image_text", "imageText")),
    smallImageKey: text(pick(raw, "smallImageKey", "small_image", "smallImage", "badgeKey")),
    smallImageText: text(pick(raw, "smallImageText", "small_image_text", "badgeText")),
    buttons: readButtons(pick(raw, "buttons", "buttonList") ?? raw),
    startTimestamp: flag(pick(raw, "startTimestamp", "timestamp", "start_time")) ?? false,
  };
}

function readPresets(raw: unknown): Preset[] {
  const list = Array.isArray(raw)
    ? raw
    : isRecord(raw) && Array.isArray(raw.presets)
      ? raw.presets
      : isRecord(raw) && Array.isArray(raw.activities)
        ? raw.activities
        : [];

  const presets: Preset[] = [];
  const seen = new Set<string>();
  for (const entry of list) {
    const preset = readPreset(entry);
    if (!preset || seen.has(preset.id)) {
      continue;
    }

    seen.add(preset.id);
    presets.push(preset);
  }

  return presets;
}

function readError(raw: RecordValue): string | null {
  const node = pick(raw, "error", "lastError", "last_error", "reason");
  if (typeof node === "string") {
    return node.trim() === "" ? null : node.trim();
  }

  if (isRecord(node)) {
    const message = text(pick(node, "message", "text", "reason")).trim();
    return message === "" ? null : message;
  }

  return null;
}

function readState(raw: unknown): DiscordState | null {
  if (!isRecord(raw)) {
    return null;
  }

  const source = isRecord(raw.state) ? { ...raw, ...raw.state } : raw;
  const statusText = text(pick(source, "status", "connection", "presence")).toLowerCase();
  const connected = flag(pick(source, "connected", "isConnected", "attached"));
  const error = readError(source);

  let status: ConnectionStatus = "off";
  if (connected === true || ["connected", "online", "ready", "running"].includes(statusText)) {
    status = "connected";
  } else if (["error", "failed", "failure"].includes(statusText) || error !== null) {
    status = "error";
  }

  const activeRaw = pick(source, "activePresetId", "activePreset", "active");
  const activePresetId =
    typeof activeRaw === "string"
      ? activeRaw
      : isRecord(activeRaw)
        ? text(pick(activeRaw, "id", "name", "key"))
        : null;

  return {
    status,
    appId: text(pick(source, "appId", "applicationId", "clientId", "app_id")),
    error,
    presets: readPresets(source),
    activePresetId: activePresetId || null,
  };
}

function readMatchKind(value: unknown): MatchKind {
  const lower = text(value).toLowerCase();
  if (lower === "class" || lower === "windowclass" || lower === "classname" || lower === "window_class") {
    return "class";
  }

  if (lower === "title" || lower === "windowtitle" || lower === "caption" || lower === "window_title") {
    return "title";
  }

  return "exe";
}

function readRules(raw: unknown): Rule[] {
  const list = Array.isArray(raw)
    ? raw
    : isRecord(raw) && Array.isArray(raw.rules)
      ? raw.rules
      : isRecord(raw) && Array.isArray(raw.items)
        ? raw.items
        : [];

  const rules: Rule[] = [];
  const seen = new Set<string>();

  for (const [index, entry] of list.entries()) {
    if (!isRecord(entry)) {
      continue;
    }

    const pattern = text(pick(entry, "pattern", "value", "match", "contains", "needle")).trim();
    if (pattern.length === 0) {
      continue;
    }

    const match = readMatchKind(pick(entry, "match", "type", "kind"));
    const presetId = text(pick(entry, "presetId", "preset", "activity", "preset_id")).trim();
    let id = text(pick(entry, "id", "key")).trim();
    if (id.length === 0) {
      id = `${match}:${pattern}:${index}`;
    }

    if (seen.has(id)) {
      continue;
    }

    seen.add(id);
    rules.push({ id, match, pattern, presetId });
  }

  return rules;
}

/** The editor always shows two button slots; empty ones are dropped before the call. */
function editorButtons(buttons: PresetButton[]): PresetButton[] {
  return [0, 1].map((index) => {
    const entry = buttons[index];
    return { label: entry?.label ?? "", url: entry?.url ?? "" };
  });
}

function emptyPreset(): Preset {
  return {
    id: "",
    name: "",
    details: "",
    state: "",
    largeImageKey: "",
    largeImageText: "",
    smallImageKey: "",
    smallImageText: "",
    buttons: editorButtons([]),
    startTimestamp: false,
  };
}

function clonePreset(preset: Preset): Preset {
  return { ...preset, buttons: editorButtons(preset.buttons) };
}

function samePreset(left: Preset, right: Preset): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function cleanPreset(preset: Preset): Preset {
  const buttons = preset.buttons
    .map((entry) => ({ label: entry.label.trim(), url: entry.url.trim() }))
    .filter((entry) => entry.label.length > 0 && entry.url.length > 0)
    .slice(0, 2);

  return {
    id: preset.id.trim(),
    name: preset.name.trim(),
    details: preset.details.trim(),
    state: preset.state.trim(),
    largeImageKey: preset.largeImageKey.trim(),
    largeImageText: preset.largeImageText.trim(),
    smallImageKey: preset.smallImageKey.trim(),
    smallImageText: preset.smallImageText.trim(),
    buttons,
    startTimestamp: preset.startTimestamp,
  };
}

const matchLabel: Record<MatchKind, MessageKey> = {
  exe: "discord.rules.type.exe",
  class: "discord.rules.type.class",
  title: "discord.rules.type.title",
};

const matchKinds: readonly MatchKind[] = ["exe", "class", "title"];

export function createDiscordScreen(ctx: AppContext): Screen {
  const t = (key: MessageKey, params?: Record<string, string | number>): string =>
    ctx.i18n.t(key, params);
  const l = (key: keyof typeof copy.ru): string => copy[ctx.i18n.language][key];

  const root = document.createElement("section");
  root.className = "screen discord-screen";
  root.dataset.screen = "discord";

  let loading = true;
  let unavailable = false;
  let failed = false;
  let state: DiscordState | null = null;
  let rules: Rule[] = [];
  let rulesUnavailable = false;
  let rulesFailed = false;

  let draft: Preset = emptyPreset();
  let draftId: string | null = null;
  let baseline: Preset = emptyPreset();
  let draftReady = false;

  let appIdInput = "";
  let appIdError = false;
  let busy: Busy = null;
  let buttonErrors: Array<MessageKey | null> = [null, null];
  let newRule: NewRule = { match: "exe", pattern: "", presetId: "" };
  let ruleError: MessageKey | null = null;

  let dirtyChip: HTMLElement | null = null;
  let saveButton: HTMLButtonElement | null = null;
  const focus = watchFocus(root);

  const presets = (): Preset[] => state?.presets ?? [];

  const presetName = (id: string): string =>
    presets().find((preset) => preset.id === id)?.name ?? t("discord.rules.missingPreset");

  const isDirty = (): boolean => !samePreset(draft, baseline);

  const syncDirty = (): void => {
    if (dirtyChip) {
      dirtyChip.hidden = !isDirty();
    }

    if (saveButton) {
      saveButton.disabled = busy !== null || draft.name.trim().length === 0;
    }
  };

  const adoptDraft = (preset: Preset | null): void => {
    draft = preset ? clonePreset(preset) : emptyPreset();
    baseline = clonePreset(draft);
    draftId = preset && preset.id.length > 0 ? preset.id : null;
    buttonErrors = [null, null];
    draftReady = true;
  };

  const load = async (quiet = false): Promise<void> => {
    if (!quiet) {
      loading = true;
      unavailable = false;
      failed = false;
      render();
    }

    const [stateResult, rulesResult] = await Promise.all([
      ctx.host.call<unknown>("discord.getState"),
      ctx.host.call<unknown>("discord.getRules"),
    ]);

    loading = false;

    if (!stateResult.ok) {
      state = null;
      rules = [];
      unavailable = stateResult.reason === "unavailable";
      failed = !unavailable;
      render();
      return;
    }

    unavailable = false;
    failed = false;
    state =
      readState(stateResult.data) ?? {
        status: "off",
        appId: "",
        error: null,
        presets: [],
        activePresetId: null,
      };

    // A host that keeps presets in their own store instead of the state payload is still usable.
    if (state.presets.length === 0) {
      const extra = await ctx.host.call<unknown>("discord.getPresets");
      if (extra.ok) {
        state.presets = readPresets(extra.data);
      }
    }

    if (appIdInput.length === 0 && state.appId.length > 0) {
      appIdInput = state.appId;
    }

    if (!draftReady) {
      const active = state.presets.find((preset) => preset.id === state?.activePresetId);
      adoptDraft(active ?? state.presets[0] ?? null);
    }

    rulesUnavailable = !rulesResult.ok && rulesResult.reason === "unavailable";
    rulesFailed = !rulesResult.ok && !rulesUnavailable;
    rules = rulesResult.ok ? readRules(rulesResult.data) : [];

    if (rules.length > 0 && newRule.presetId.length === 0) {
      const firstId = rules[0]?.presetId;
      if (firstId && presets().some((preset) => preset.id === firstId)) {
        newRule = { ...newRule, presetId: firstId };
      }
    }

    render();
  };

  const failToast = (
    result: Extract<CallResult<unknown>, { ok: false }>,
    key: MessageKey,
  ): void => {
    showToast(t(result.reason === "unavailable" ? "toast.unavailable" : key), "error");
  };

  const connect = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    const appId = appIdInput.trim();
    if (!validAppId(appId)) {
      appIdError = true;
      render();
      root.querySelector<HTMLInputElement>("#discord-app-id")?.focus();
      return;
    }

    appIdError = false;
    busy = "connect";
    render();

    const result = await ctx.host.call<unknown>("discord.connect", { appId, applicationId: appId });
    busy = null;

    if (!result.ok) {
      failToast(result, "discord.connectFailed");
      render();
      return;
    }

    showToast(t("discord.connectedToast"), "success");
    await load(true);
  };

  /**
   * `discord.configure`: persist the id on its own, without waiting for a connection.
   *
   * Saving is a different promise from connecting: the id can be valid while Discord is closed, so
   * the screen reports the host's new state and leaves the connect button to the user.
   */
  const saveAppId = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    const appId = appIdInput.trim();
    if (!validAppId(appId)) {
      appIdError = true;
      render();
      root.querySelector<HTMLInputElement>("#discord-app-id")?.focus();
      return;
    }

    appIdError = false;
    busy = "saveAppId";
    render();

    const result = await ctx.host.call<unknown>("discord.configure", { appId });
    busy = null;

    if (!result.ok) {
      showToast(
        result.reason === "unavailable" ? t("toast.unavailable") : l("saveFailed"),
        "error",
      );
      render();
      return;
    }

    const next = readState(result.data);
    if (next) {
      state = next;
      appIdInput = next.appId || appId;
    }

    showToast(t("common.saved"), "success");
    await load(true);
  };

  const disconnect = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "disconnect";
    render();

    const result = await ctx.host.call<unknown>("discord.disconnect");
    busy = null;

    if (!result.ok) {
      failToast(result, "discord.disconnectFailed");
      render();
      return;
    }

    showToast(t("discord.disconnectedToast"), "success");
    await load(true);
  };

  const activate = async (preset: Preset): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "activate";
    render();

    const result = await ctx.host.call<unknown>("discord.setPreset", {
      id: preset.id,
      presetId: preset.id,
    });
    busy = null;

    if (!result.ok) {
      failToast(result, "discord.presets.activateFailed");
      render();
      return;
    }

    showToast(t("discord.presets.activated"), "success");
    await load(true);
  };

  const validateButtons = (): boolean => {
    buttonErrors = draft.buttons.map((entry) => {
      const label = entry.label.trim();
      const url = entry.url.trim();
      if (label.length === 0 && url.length === 0) {
        return null;
      }

      if (label.length === 0 || url.length === 0) {
        return "discord.editor.invalidPair";
      }

      if (!/^https:\/\//i.test(url)) {
        return "discord.editor.invalidUrl";
      }

      return null;
    });

    while (buttonErrors.length < 2) {
      buttonErrors.push(null);
    }

    return buttonErrors.every((entry) => entry === null);
  };

  const savePreset = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    if (draft.name.trim().length === 0) {
      showToast(t("discord.presets.nameRequired"), "error");
      root.querySelector<HTMLInputElement>("#discord-preset-name")?.focus();
      return;
    }

    if (!validateButtons()) {
      render();
      const first = buttonErrors.find((entry) => entry !== null);
      showToast(t(first ?? "discord.presets.saveFailed"), "error");
      return;
    }

    busy = "save";
    render();

    const value = cleanPreset({ ...draft, id: draftId ?? draft.id });
    const result = await ctx.host.call<unknown>("discord.savePreset", {
      ...value,
      preset: value,
    });
    busy = null;

    if (!result.ok) {
      failToast(result, "discord.presets.saveFailed");
      render();
      return;
    }

    const payload = isRecord(result.data) ? result.data : {};
    const saved = readPreset(pick(payload, "preset", "activity") ?? payload);
    adoptDraft(saved ?? value);
    showToast(t("discord.presets.saved"), "success");
    await load(true);
  };

  const deletePreset = async (): Promise<void> => {
    if (busy !== null || !draftId) {
      return;
    }

    const confirmed = await confirmDialog({
      title: t("discord.presets.deleteTitle", { name: draft.name }),
      text: t("discord.presets.deleteText"),
      confirm: t("discord.presets.deleteConfirm"),
      cancel: t("common.cancel"),
      danger: true,
    });

    if (!confirmed) {
      return;
    }

    busy = "delete";
    render();

    const result = await ctx.host.call<unknown>("discord.savePreset", {
      id: draftId,
      preset: { id: draftId },
      delete: true,
      remove: true,
    });
    busy = null;

    if (!result.ok) {
      failToast(result, "discord.presets.deleteFailed");
      render();
      return;
    }

    adoptDraft(null);
    showToast(t("discord.presets.deleted"), "success");
    await load(true);
  };

  const saveRules = async (next: Rule[], toastKey: MessageKey): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "rules";
    render();

    const result = await ctx.host.call<unknown>("discord.setRules", { rules: next });
    busy = null;

    if (!result.ok) {
      failToast(result, "discord.rules.saveFailed");
      render();
      return;
    }

    const echoed = readRules(result.data);
    rules = echoed.length > 0 ? echoed : next;
    showToast(t(toastKey), "success");
    render();
  };

  const addRule = async (): Promise<void> => {
    const pattern = newRule.pattern.trim();
    if (pattern.length === 0) {
      ruleError = "discord.rules.needPattern";
      render();
      root.querySelector<HTMLInputElement>("#discord-rule-pattern")?.focus();
      return;
    }

    const presetId =
      newRule.presetId ||
      state?.activePresetId ||
      presets()[0]?.id ||
      "";

    if (presetId.length === 0) {
      ruleError = "discord.rules.needPreset";
      render();
      return;
    }

    ruleError = null;
    const id = `r${Date.now().toString(36)}${rules.length.toString(36)}`;
    const next = [...rules, { id, match: newRule.match, pattern, presetId }];
    newRule = { ...newRule, pattern: "" };
    await saveRules(next, "discord.rules.added");
  };

  const removeRule = async (rule: Rule): Promise<void> => {
    await saveRules(
      rules.filter((entry) => entry.id !== rule.id),
      "discord.rules.removed",
    );
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", t("discord.title")),
      el("p", "screen__subtitle", t("discord.subtitle")),
    );

    const aside = el("div", "screen__aside");
    const status = state?.status ?? "off";
    aside.append(
      pill(
        t(
          status === "connected"
            ? "discord.status.connected"
            : status === "error"
              ? "discord.status.error"
              : "discord.status.notConfigured",
        ),
        status === "connected" ? "on" : status === "error" ? "danger" : "neutral",
      ),
    );

    head.append(copy, aside);
    return head;
  };

  const renderConnection = (): HTMLElement => {
    const card = panel({
      title: t("discord.status.title"),
      hint: t("discord.status.hint"),
      icon: "discord",
      className: "discord__connection",
    });

    const status = state?.status ?? "off";
    const row = el("div", "discord-status");
    const dot = el("span", "discord-status__dot");
    dot.classList.add(`discord-status__dot--${status}`);
    row.append(
      dot,
      el(
        "span",
        "discord-status__text",
        t(
          status === "connected"
            ? "discord.status.connected"
            : status === "error"
              ? "discord.status.error"
              : "discord.status.notConfigured",
        ),
      ),
    );
    card.body.append(row);

    if (state?.error) {
      card.body.append(el("p", "discord__error", t("discord.error", { message: state.error })));
    }

    const labelRow = el("div", "field__labelRow");
    const label = el("label", "field__label", t("discord.appId"));
    label.htmlFor = "discord-app-id";
    labelRow.append(label);

    const where = el("button", "field__link", l("where"));
    where.type = "button";
    where.id = "discord-app-id-help";
    where.addEventListener("click", () => {
      window.open(
        "https://discord.com/developers/applications",
        "_blank",
        "noopener,noreferrer",
      );
    });
    labelRow.append(where);

    const input = document.createElement("input");
    input.type = "text";
    input.id = "discord-app-id";
    input.className = "input input--mono";
    input.inputMode = "numeric";
    input.spellcheck = false;
    input.autocomplete = "off";
    input.placeholder = t("discord.appId.placeholder");
    input.value = appIdInput;
    input.disabled = busy !== null;
    input.setAttribute("aria-describedby", "discord-app-id-error");
    if (appIdError) {
      input.setAttribute("aria-invalid", "true");
      input.setAttribute("aria-errormessage", "discord-app-id-error");
    }

    input.addEventListener("input", () => {
      appIdInput = input.value;
      if (appIdError && validAppId(input.value)) {
        appIdError = false;
        input.removeAttribute("aria-invalid");
        input.removeAttribute("aria-errormessage");
        card.el.querySelector<HTMLElement>(".field__error")?.setAttribute("hidden", "");
      }

      // The buttons are drawn before the first keystroke; enable them without a full re-render.
      const connectButton = card.el.querySelector<HTMLButtonElement>("#discord-connect");
      if (connectButton) {
        connectButton.disabled = busy !== null || !validAppId(input.value);
      }

      const saveButton = card.el.querySelector<HTMLButtonElement>("#discord-save-app-id");
      if (saveButton) {
        saveButton.disabled = busy !== null || !validAppId(input.value);
      }
    });

    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void saveAppId();
      }
    });

    const error = el("span", "field__error", t("discord.appId.invalid"));
    error.id = "discord-app-id-error";
    error.setAttribute("role", "alert");
    error.hidden = !appIdError;

    card.body.append(
      labelRow,
      input,
      el("p", "panel__note", t("discord.appId.hint")),
      error,
    );

    if (state?.appId) {
      const saved = el("div", "discord__savedId");
      saved.append(
        el("span", "discord__savedIdLabel", l("savedId")),
        el("code", "discord__savedIdValue", state.appId),
      );
      card.body.append(saved);
    }

    const actions = el("div", "discord__actions");

    const save = button({
      label: busy === "saveAppId" ? l("savingAppId") : l("saveAppId"),
      variant: "secondary",
      icon: "check",
      onClick: () => void saveAppId(),
    });
    save.id = "discord-save-app-id";
    save.disabled = busy !== null || !validAppId(appIdInput);
    if (busy === "saveAppId") {
      setBusy(save, true, l("savingAppId"));
    }
    actions.append(save);

    if (status === "connected") {
      const off = button({
        label: busy === "disconnect" ? t("discord.disconnecting") : t("discord.disconnect"),
        variant: "secondary",
        icon: "power",
        onClick: () => void disconnect(),
      });
      off.id = "discord-disconnect";
      off.disabled = busy !== null;
      if (busy === "disconnect") {
        setBusy(off, true, t("discord.disconnecting"));
      }

      actions.append(off);
    } else {
      const on = button({
        label: busy === "connect" ? t("discord.connecting") : t("discord.connect"),
        variant: "primary",
        icon: "discord",
        onClick: () => void connect(),
      });
      on.id = "discord-connect";
      on.disabled = busy !== null || !validAppId(appIdInput);
      if (busy === "connect") {
        setBusy(on, true, t("discord.connecting"));
      }

      actions.append(on);
    }

    card.body.append(actions);

    if (status !== "connected") {
      card.body.append(el("p", "panel__note", t("discord.offHint")));
    }

    return card.el;
  };

  const renderPresets = (): HTMLElement => {
    const fresh = button({
      label: t("discord.presets.new"),
      variant: "ghost",
      size: "sm",
      icon: "plus",
      onClick: () => {
        adoptDraft(null);
        render();
        root.querySelector<HTMLInputElement>("#discord-preset-name")?.focus();
      },
    });
    fresh.id = "discord-new-preset";

    const card = panel({
      title: t("discord.presets.title"),
      hint: t("discord.presets.hint"),
      icon: "profiles",
      actions: [fresh],
      className: "discord__presets",
    });

    if (presets().length === 0) {
      card.body.append(
        emptyState({
          icon: "discord",
          title: t("discord.presets.emptyTitle"),
          text: t("discord.presets.empty"),
          action: button({
            label: t("discord.presets.new"),
            variant: "secondary",
            size: "sm",
            icon: "plus",
            onClick: () => {
              adoptDraft(null);
              render();
              root.querySelector<HTMLInputElement>("#discord-preset-name")?.focus();
            },
          }),
        }),
      );
      return card.el;
    }

    const list = el("div", "preset-list");
    list.setAttribute("role", "list");

    for (const preset of presets()) {
      const active = preset.id === state?.activePresetId;
      const editing = preset.id === draftId;
      const row = el("div", `preset-row${active ? " preset-row--active" : ""}${editing ? " preset-row--editing" : ""}`);
      row.setAttribute("role", "listitem");

      const open = el("button", "preset-row__open");
      open.type = "button";
      open.dataset.rovingItem = "";
      open.dataset.focus = `preset-${preset.id}`;
      open.dataset.preset = preset.id;
      open.setAttribute("aria-pressed", String(editing));

      const copy = el("span", "preset-row__copy");
      copy.append(el("span", "preset-row__name", preset.name));
      const meta = preset.details || preset.state;
      if (meta) {
        copy.append(el("span", "preset-row__meta", meta));
      }

      const marks = el("span", "preset-row__marks");
      if (active) {
        marks.append(pill(t("discord.presets.active"), "accent"));
      }

      open.append(copy, marks);
      open.addEventListener("click", () => {
        adoptDraft(preset);
        render();
        root.querySelector<HTMLInputElement>("#discord-preset-name")?.focus();
      });

      row.append(open);

      if (!active) {
        const makeActive = button({
          label: t("discord.presets.activate"),
          variant: "ghost",
          size: "sm",
          icon: "check",
          onClick: () => void activate(preset),
        });
        makeActive.dataset.focus = `activate-preset-${preset.id}`;
        makeActive.disabled = busy !== null;
        row.append(makeActive);
      }

      list.append(row);
    }

    attachRovingKeys(list);
    card.body.append(list);
    card.body.append(
      el(
        "p",
        "panel__note",
        countLabel(ctx.i18n.language, presets().length, nouns.presets.ru, nouns.presets.en),
      ),
    );
    return card.el;
  };

  const textField = (options: {
    id: string;
    label: string;
    value: string;
    placeholder?: string;
    mono?: boolean;
    full?: boolean;
    onInput(value: string): void;
    onEnter?(): void;
  }): HTMLElement => {
    const wrap = el("div", `field${options.full ? " field--wide" : ""}`);
    const label = el("label", "field__label", options.label);
    label.htmlFor = options.id;

    const input = document.createElement("input");
    input.type = "text";
    input.id = options.id;
    input.className = `input${options.mono ? " input--mono" : ""}`;
    input.spellcheck = false;
    input.autocomplete = "off";
    input.value = options.value;
    input.disabled = busy !== null;
    if (options.placeholder) {
      input.placeholder = options.placeholder;
    }

    input.addEventListener("input", () => {
      options.onInput(input.value);
      syncDirty();
    });

    if (options.onEnter) {
      input.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          options.onEnter?.();
        }
      });
    }

    wrap.append(label, input);
    return wrap;
  };

  const renderButtonsEditor = (): HTMLElement => {
    const block = el("div", "discord-buttons");
    block.append(el("span", "panel__subhead", t("discord.editor.buttons")));
    block.append(el("p", "panel__note", t("discord.editor.buttonsHint")));

    for (let index = 0; index < 2; index += 1) {
      const entry = draft.buttons[index] ?? { label: "", url: "" };
      const slot = el("div", "discord-buttons__slot");
      slot.dataset.slot = String(index + 1);
      slot.append(
        el("span", "discord-buttons__title", t("discord.editor.button", { n: index + 1 })),
      );

      const fields = el("div", "discord-buttons__fields");
      fields.append(
        textField({
          id: `discord-button-label-${index}`,
          label: t("discord.editor.buttonLabel"),
          value: entry.label,
          placeholder: "ProjectNeb",
          onInput: (value) => {
            const buttons = [...draft.buttons];
            buttons[index] = { label: value, url: buttons[index]?.url ?? "" };
            draft.buttons = buttons;
          },
        }),
        textField({
          id: `discord-button-url-${index}`,
          label: t("discord.editor.buttonUrl"),
          value: entry.url,
          placeholder: "https://github.com/temajmix/ProjectNeb",
          mono: true,
          onInput: (value) => {
            const buttons = [...draft.buttons];
            buttons[index] = { label: buttons[index]?.label ?? "", url: value };
            draft.buttons = buttons;
          },
        }),
      );

      slot.append(fields);

      const error = buttonErrors[index];
      if (error) {
        const message = el("span", "field__error", t(error));
        message.id = `discord-button-${index}-error`;
        message.setAttribute("role", "alert");
        slot.append(message);
        for (const control of fields.querySelectorAll<HTMLInputElement>("input")) {
          control.setAttribute("aria-describedby", message.id);
          control.setAttribute("aria-errormessage", message.id);
          control.setAttribute("aria-invalid", "true");
        }
      }

      block.append(slot);
    }

    return block;
  };

  const renderEditor = (): HTMLElement => {
    const title = draftId ? t("discord.editor.title") : t("discord.editor.newTitle");
    dirtyChip = pill(t("discord.editor.dirty"), "warn");
    dirtyChip.hidden = !isDirty();
    dirtyChip.id = "discord-dirty";

    saveButton = button({
      label: busy === "save" ? t("discord.editor.saving") : t("discord.editor.save"),
      variant: "primary",
      icon: "check",
      onClick: () => void savePreset(),
    });
    saveButton.id = "discord-save";

    const card = panel({
      title,
      hint: t("discord.editor.hint"),
      icon: "discord",
      actions: [dirtyChip],
      className: "discord__editor",
    });

    const form = el("div", "discord-form");
    form.append(
      textField({
        id: "discord-preset-name",
        label: t("discord.editor.name"),
        value: draft.name,
        placeholder: "ProjectNeb",
        full: true,
        onInput: (value) => {
          draft.name = value;
        },
      }),
      textField({
        id: "discord-preset-details",
        label: t("discord.editor.details"),
        value: draft.details,
        placeholder: t("app.tagline"),
        full: true,
        onInput: (value) => {
          draft.details = value;
        },
      }),
      textField({
        id: "discord-preset-state",
        label: t("discord.editor.state"),
        value: draft.state,
        placeholder: "ProjectNeb",
        full: true,
        onInput: (value) => {
          draft.state = value;
        },
      }),
      textField({
        id: "discord-large-image",
        label: t("discord.editor.largeImage"),
        value: draft.largeImageKey,
        placeholder: "neb",
        mono: true,
        onInput: (value) => {
          draft.largeImageKey = value;
        },
      }),
      textField({
        id: "discord-large-text",
        label: t("discord.editor.largeText"),
        value: draft.largeImageText,
        placeholder: "ProjectNeb",
        onInput: (value) => {
          draft.largeImageText = value;
        },
      }),
      textField({
        id: "discord-small-image",
        label: t("discord.editor.smallImage"),
        value: draft.smallImageKey,
        placeholder: "beta",
        mono: true,
        onInput: (value) => {
          draft.smallImageKey = value;
        },
      }),
      textField({
        id: "discord-small-text",
        label: t("discord.editor.smallText"),
        value: draft.smallImageText,
        placeholder: "beta",
        onInput: (value) => {
          draft.smallImageText = value;
        },
      }),
    );

    const timestamp = el("div", "settings__row discord__timestamp");
    const clock = el("span", "settings__icon");
    clock.innerHTML = '<svg class="icon" width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><circle cx="12" cy="12" r="8.2"/><path d="M12 7.6V12l3.1 2"/></svg>';
    const copy = el("span", "settings__text");
    copy.append(
      el("span", "settings__name", t("discord.editor.timestamp")),
      el("span", "settings__desc", t("discord.editor.timestampHint")),
    );

    timestamp.append(
      clock,
      copy,
      switchControl({
        checked: draft.startTimestamp,
        label: t("discord.editor.timestamp"),
        id: "discord-timestamp",
        disabled: busy !== null,
        onToggle: (next) => {
          draft.startTimestamp = next;
          syncDirty();
          render();
        },
      }),
    );

    card.body.append(form, renderButtonsEditor(), timestamp);

    const actions = el("div", "discord__actions discord__actions--editor");
    actions.append(saveButton);
    if (draftId) {
      const remove = button({
        label: t("discord.editor.delete"),
        variant: "ghost",
        icon: "trash",
        onClick: () => void deletePreset(),
      });
      remove.id = "discord-delete";
      remove.disabled = busy !== null;
      actions.append(remove);
    }

    card.body.append(actions);
    syncDirty();
    return card.el;
  };

  const renderRules = (): HTMLElement => {
    const count = rules.length;
    const card = panel({
      title: t("discord.rules.title"),
      hint: t("discord.rules.hint"),
      icon: "settings",
      actions: [
        pill(
          countLabel(ctx.i18n.language, count, nouns.rules.ru, nouns.rules.en),
          "neutral",
        ),
      ],
      className: "discord__rules",
    });

    if (rulesUnavailable) {
      card.body.append(el("p", "state-note", t("common.unavailable")));
      return card.el;
    }

    if (rulesFailed) {
      card.body.append(el("p", "state-note", t("discord.failed.text")));
      return card.el;
    }

    const form = el("div", "rule-form");

    const kindLabel = el("label", "field__label", t("discord.rules.match"));
    kindLabel.htmlFor = "discord-rule-match";
    const kind = document.createElement("select");
    kind.className = "select";
    kind.id = "discord-rule-match";
    kind.disabled = busy !== null;
    for (const value of matchKinds) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = t(matchLabel[value]);
      option.selected = value === newRule.match;
      kind.append(option);
    }

    kind.addEventListener("change", () => {
      newRule = { ...newRule, match: readMatchKind(kind.value) };
    });

    const kindField = el("div", "field rule-form__match");
    kindField.append(kindLabel, kind);

    const pattern = document.createElement("input");
    pattern.type = "text";
    pattern.id = "discord-rule-pattern";
    pattern.className = "input";
    pattern.spellcheck = false;
    pattern.autocomplete = "off";
    pattern.placeholder = t("discord.rules.patternPlaceholder");
    pattern.value = newRule.pattern;
    pattern.disabled = busy !== null;

    pattern.addEventListener("input", () => {
      newRule = { ...newRule, pattern: pattern.value };
      if (ruleError) {
        ruleError = null;
        pattern.removeAttribute("aria-invalid");
        pattern.removeAttribute("aria-describedby");
        pattern.removeAttribute("aria-errormessage");
        const message = root.querySelector<HTMLElement>("#discord-rule-error");
        if (message) {
          message.hidden = true;
        }
      }
    });

    pattern.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void addRule();
      }
    });

    const patternLabel = el("label", "field__label", t("discord.rules.pattern"));
    patternLabel.htmlFor = "discord-rule-pattern";
    const patternField = el("div", "field rule-form__pattern");
    patternField.append(patternLabel, pattern);

    const presetLabel = el("label", "field__label", t("discord.rules.preset"));
    presetLabel.htmlFor = "discord-rule-preset";

    const presetSelect = document.createElement("select");
    presetSelect.className = "select";
    presetSelect.id = "discord-rule-preset";
    presetSelect.disabled = busy !== null || presets().length === 0;

    const fallback = document.createElement("option");
    fallback.value = "";
    fallback.textContent = t("discord.rules.anyPreset");
    fallback.selected = newRule.presetId.length === 0;
    presetSelect.append(fallback);

    for (const preset of presets()) {
      const option = document.createElement("option");
      option.value = preset.id;
      option.textContent = preset.name;
      option.selected = preset.id === newRule.presetId;
      presetSelect.append(option);
    }

    presetSelect.addEventListener("change", () => {
      newRule = { ...newRule, presetId: presetSelect.value };
    });

    const presetField = el("div", "field rule-form__preset");
    presetField.append(presetLabel, presetSelect);

    const add = button({
      label: t("discord.rules.add"),
      variant: "secondary",
      size: "sm",
      icon: "plus",
      onClick: () => void addRule(),
    });
    add.id = "discord-add-rule";
    add.disabled = busy !== null || presets().length === 0;

    const addField = el("div", "rule-form__action");
    const addLabel = el("span", "field__label", "\u00a0");
    addLabel.setAttribute("aria-hidden", "true");
    addField.append(addLabel, add);

    form.append(kindField, patternField, presetField, addField);
    card.body.append(form);

    const error = el("p", "field__error", ruleError ? t(ruleError) : "");
    error.id = "discord-rule-error";
    error.setAttribute("role", "alert");
    error.hidden = ruleError === null;
    if (ruleError) {
      pattern.setAttribute("aria-invalid", "true");
      pattern.setAttribute("aria-describedby", error.id);
      pattern.setAttribute("aria-errormessage", error.id);
    }

    card.body.append(error);

    if (presets().length === 0) {
      card.body.append(el("p", "panel__note", t("discord.rules.needPreset")));
    }

    if (rules.length === 0) {
      card.body.append(el("p", "panel__note", t("discord.rules.empty")));
      return card.el;
    }

    const list = el("div", "rule-list");
    list.setAttribute("role", "list");
    for (const rule of rules) {
      const row = el("div", "rule-row");
      row.setAttribute("role", "listitem");
      const copy = el("span", "rule-row__copy");
      copy.append(
        el(
          "span",
          "rule-row__match",
          t("discord.rules.summary", {
            type: t(matchLabel[rule.match]),
            pattern: rule.pattern,
          }),
        ),
        el("span", "rule-row__preset", presetName(rule.presetId)),
      );

      const remove = iconButton(
        "trash",
        t("discord.rules.remove"),
        () => void removeRule(rule),
        "icon-btn--danger",
      );
      remove.dataset.rovingItem = "";
      remove.dataset.focus = `rule-${rule.id}`;
      remove.disabled = busy !== null;

      row.append(copy, remove);
      list.append(row);
    }

    attachRovingKeys(list);
    card.body.append(list);
    return card.el;
  };

  const renderMessage = (title: string, body: string): HTMLElement => {
    const card = panel({});
    const note = el("div", "state-note");
    note.append(el("strong", "state-note__title", title), el("span", "state-note__text", body));
    card.body.append(
      note,
      button({
        label: t("common.retry"),
        variant: "secondary",
        size: "sm",
        icon: "refresh",
        onClick: () => void load(),
      }),
    );
    return card.el;
  };

  const render = (): void => {
    if (loading) {
      const grid = el("div", "discord__grid");
      grid.append(skeletonPanel(5), skeletonPanel(6));
      root.replaceChildren(renderHeader(), grid);
      return;
    }

    if (unavailable) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(t("discord.unavailable.title"), t("discord.unavailable.text")),
      );
      return;
    }

    if (failed || !state) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(t("discord.failed.title"), t("discord.failed.text")),
      );
      return;
    }

    const grid = el("div", "discord__grid");
    const side = el("div", "discord__side");
    side.append(renderConnection(), renderPresets());
    grid.append(side, renderEditor());

    root.replaceChildren(renderHeader(), grid, renderRules());
    focus.restore();
  };

  const activateScreen = async (): Promise<void> => {
    await load();
  };

  return { el: root, activate: activateScreen, render };
}
