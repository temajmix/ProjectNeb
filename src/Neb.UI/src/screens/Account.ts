import type { AppContext, Screen } from "../context";
import type { CallResult } from "../host";
import type { MessageKey } from "../i18n";
import { button, el, panel, pill, setBusy } from "../components/controls";
import { icon } from "../components/icons";
import { showToast } from "../components/toast";

/**
 * «Аккаунт» — the ProjectNeb Pro account.
 *
 * Everything on this screen is read from `account.getState`; the host owns the tokens and never
 * hands them to the page. The sign-in button is disabled while `account.apiBase` is empty, because
 * a button that opens nothing is worse than a sentence that says what to configure.
 */

type RecordValue = Record<string, unknown>;
type Busy = "signIn" | "signOut" | "refresh" | null;
type Plan = "free" | "pro";

interface AccountState {
  signedIn: boolean;
  plan: Plan;
  until: string | null;
  name: string | null;
  avatar: string | null;
  stale: boolean;
  configured: boolean;
}

function isRecord(value: unknown): value is RecordValue {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function textOf(value: unknown): string | null {
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim();
  return trimmed.length === 0 ? null : trimmed;
}

function readState(raw: unknown): AccountState {
  const source = isRecord(raw) ? raw : {};
  return {
    signedIn: source.signedIn === true,
    plan: source.plan === "pro" ? "pro" : "free",
    until: textOf(source.until),
    name: textOf(source.name),
    avatar: textOf(source.avatar),
    stale: source.stale === true,
    configured: source.configured === true,
  };
}

const emptyState = (): AccountState => ({
  signedIn: false,
  plan: "free",
  until: null,
  name: null,
  avatar: null,
  stale: false,
  configured: false,
});

export function createAccountScreen(ctx: AppContext): Screen {
  const t = (key: MessageKey, params?: Record<string, string | number>): string =>
    ctx.i18n.t(key, params);

  const root = document.createElement("section");
  root.className = "screen account";
  root.dataset.screen = "account";

  let loading = true;
  let failed = false;
  let state: AccountState = emptyState();
  let busy: Busy = null;

  const formatUntil = (iso: string | null): string | null => {
    if (!iso) {
      return null;
    }

    const date = new Date(iso);
    if (Number.isNaN(date.getTime())) {
      return null;
    }

    return new Intl.DateTimeFormat(ctx.i18n.language === "en" ? "en-US" : "ru-RU", {
      dateStyle: "long",
    }).format(date);
  };

  const planLabel = (plan: Plan): string =>
    plan === "pro" ? t("account.plan.pro") : t("account.plan.free");

  const load = async (quiet = false): Promise<void> => {
    if (!quiet) {
      loading = true;
      failed = false;
      render();
    }

    const result = await ctx.host.call<unknown>("account.getState");
    loading = false;

    if (!result.ok) {
      failed = true;
      render();
      return;
    }

    failed = false;
    state = readState(result.data);
    render();
  };

  const failToast = (
    result: Extract<CallResult<unknown>, { ok: false }>,
    key: MessageKey,
  ): void => {
    showToast(t(result.reason === "unavailable" ? "common.unavailable" : key), "error");
  };

  const signIn = async (): Promise<void> => {
    if (busy !== null || !state.configured) {
      return;
    }

    busy = "signIn";
    render();

    const result = await ctx.host.call<unknown>("account.signIn");
    busy = null;

    if (!result.ok) {
      failToast(result, "account.signInFailed");
      render();
      return;
    }

    state = readState(result.data);
    render();
  };

  const signOut = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "signOut";
    render();

    const result = await ctx.host.call<unknown>("account.signOut");
    busy = null;

    if (!result.ok) {
      failToast(result, "account.signOutFailed");
      render();
      return;
    }

    state = readState(result.data);
    showToast(t("account.signedOut"), "success");
    render();
  };

  const refresh = async (): Promise<void> => {
    if (busy !== null) {
      return;
    }

    busy = "refresh";
    render();

    const result = await ctx.host.call<unknown>("account.refresh");
    busy = null;

    if (!result.ok) {
      failToast(result, "account.refreshFailed");
      await load(true);
      return;
    }

    state = readState(result.data);
    showToast(t("account.refreshed"), "success");
    render();
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", t("account.title")),
      el("p", "screen__subtitle", t("account.subtitle")),
    );

    const aside = el("div", "screen__aside");
    if (!state.configured) {
      aside.append(pill(t("account.notConfigured"), "neutral"));
    } else if (state.stale) {
      aside.append(pill(t("account.stale"), "warn"));
    } else if (state.signedIn) {
      aside.append(pill(planLabel(state.plan), state.plan === "pro" ? "accent" : "neutral"));
    } else {
      aside.append(pill(t("account.notConnected"), "neutral"));
    }

    head.append(copy, aside);
    return head;
  };

  const renderIdentity = (): HTMLElement => {
    const avatar = el("span", "plan-card__avatar");
    avatar.setAttribute("role", "img");
    avatar.setAttribute("aria-label", t("account.title"));
    const initial = state.name?.trim().charAt(0).toUpperCase();
    if (initial) {
      avatar.textContent = initial;
    } else {
      avatar.innerHTML = icon("discord", 26);
    }

    const identity = el("div", "plan-card__identity");
    identity.append(
      el(
        "span",
        "plan-card__state",
        state.signedIn ? (state.name ?? t("account.signedIn")) : t("account.notConnected"),
      ),
      el("span", "plan-card__plan", planLabel(state.plan)),
    );

    const top = el("div", "plan-card__top");
    top.append(avatar, identity);
    return top;
  };

  const renderPlan = (): HTMLElement => {
    const card = el("article", "card plan-card");

    const glow = el("span", "plan-card__glow");
    glow.setAttribute("aria-hidden", "true");

    const label = el("span", "plan-card__label", t("account.plan"));
    const desc = el(
      "p",
      "plan-card__desc",
      state.plan === "pro" ? t("account.plan.proDesc") : t("account.plan.freeDesc"),
    );

    card.append(glow, label, renderIdentity(), desc);

    if (state.stale) {
      card.append(pill(t("account.stale"), "warn"));
      card.append(el("p", "plan-card__note", t("account.staleHint")));
    }

    const until = formatUntil(state.until);
    if (until) {
      card.append(el("p", "plan-card__note", t("account.until", { date: until })));
    }

    const actions = el("div", "plan-card__actions");
    if (state.signedIn) {
      const update = button({
        label: busy === "refresh" ? t("account.refreshing") : t("account.refresh"),
        variant: "secondary",
        size: "sm",
        icon: "refresh",
        onClick: () => void refresh(),
      });
      update.id = "account-refresh";
      update.disabled = busy !== null;
      if (busy === "refresh") {
        setBusy(update, true, t("account.refreshing"));
      }

      const out = button({
        label: busy === "signOut" ? t("account.signingOut") : t("account.signOut"),
        variant: "ghost",
        size: "sm",
        icon: "power",
        onClick: () => void signOut(),
      });
      out.id = "account-signout";
      out.disabled = busy !== null;

      actions.append(update, out);
    } else {
      const login = button({
        label: busy === "signIn" ? t("account.signingIn") : t("account.login"),
        variant: "primary",
        icon: "discord",
        title: state.configured ? undefined : t("account.apiBaseHint"),
        onClick: () => void signIn(),
      });
      login.id = "account-login";
      login.disabled = busy !== null || !state.configured;
      if (busy === "signIn") {
        setBusy(login, true, t("account.signingIn"));
      }

      actions.append(login);
    }

    card.append(actions);

    if (!state.configured) {
      card.append(el("p", "plan-card__note", t("account.apiBaseHint")));
    } else if (!state.signedIn) {
      card.append(el("p", "plan-card__note", t("account.loginNote")));
    }

    const payment = el(
      "p",
      "account__payment plan-card__payment",
      t("account.payment"),
    );
    card.append(payment);

    return card;
  };

  const renderPro = (): HTMLElement => {
    const card = panel({
      title: t("account.pro.title"),
      icon: "sparkles",
      className: "account__pro",
    });

    const badge = el("span", "pro-badge", "PRO");
    badge.setAttribute("aria-hidden", "true");

    const list = el("ul", "pro-list");
    const items = [
      "account.pro.sync",
      "account.pro.store",
      "account.pro.automation",
      "account.pro.beta",
    ] as const;

    for (const key of items) {
      const item = el("li", "pro-list__item");
      const check = el("span", "pro-list__check");
      check.innerHTML = icon("check", 13);
      item.append(check, el("span", "pro-list__text", t(key)));
      list.append(item);
    }

    const note = el("p", "panel__note", t("account.pro.note"));

    card.body.append(badge, list, note);
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
      root.replaceChildren(renderHeader());
      return;
    }

    if (failed) {
      root.replaceChildren(
        renderHeader(),
        renderMessage(t("account.failed.title"), t("account.failed.text")),
      );
      return;
    }

    const grid = el("div", "account__grid");
    grid.append(renderPlan(), renderPro());
    root.replaceChildren(renderHeader(), grid);
  };

  render();

  return {
    el: root,
    activate: () => load(),
    render,
  };
}
