import type { AppContext, Screen } from "../context";
import { button, el, panel, pill } from "../components/controls";
import { icon } from "../components/icons";
import { showToast } from "../components/toast";

const RepositoryUrl = "https://github.com/temajmix/ProjectNeb";

export function createAboutScreen(ctx: AppContext): Screen {
  const root = document.createElement("section");
  root.className = "screen about-screen";
  root.dataset.screen = "about";

  let checking = false;

  const check = async (node: HTMLButtonElement): Promise<void> => {
    if (checking) {
      return;
    }

    checking = true;
    node.disabled = true;
    node.classList.add("is-busy");

    const verdict = await ctx.checkUpdates();
    checking = false;
    node.disabled = false;
    node.classList.remove("is-busy");

    if (!verdict) {
      return;
    }

    showToast(
      verdict.available
        ? ctx.i18n.t("updates.result.available", { version: verdict.version ?? "" })
        : ctx.i18n.t("updates.result.none"),
      "success",
    );
  };

  const openChangelog = (): void => {
    void (async () => {
      const result = await ctx.host.call<unknown>("app.openChangelog");
      if (!result.ok) {
        ctx.notifyFailure(result);
      }
    })();
  };

  const renderHeader = (): HTMLElement => {
    const head = el("header", "screen__head");
    const copy = el("div", "screen__copy");
    copy.append(
      el("h1", "screen__title", ctx.i18n.t("about.title")),
      el("p", "screen__subtitle", ctx.i18n.t("about.subtitle")),
    );
    head.append(copy);
    return head;
  };

  const renderHero = (): HTMLElement => {
    const card = el("article", "card about-hero");

    const mark = el("span", "about-hero__mark");
    mark.setAttribute("role", "img");
    mark.setAttribute("aria-label", ctx.i18n.t("titlebar.logo"));

    const copy = el("div", "about-hero__copy");
    const nameRow = el("div", "about-hero__nameRow");
    nameRow.append(
      el("h2", "about-hero__name", ctx.i18n.t("app.name")),
      el("span", "about-hero__version", `v${ctx.version()}`),
      pill(ctx.i18n.t(ctx.channel() === "stable" ? "updates.stable" : "updates.beta"), "accent"),
    );

    copy.append(
      nameRow,
      el("p", "about-hero__tagline", ctx.i18n.t("app.tagline")),
      el("p", "about-hero__description", ctx.i18n.t("app.description")),
    );

    const actions = el("div", "about-hero__actions");
    const checkButton = button({
      label: ctx.i18n.t("about.check"),
      variant: "primary",
      icon: "download",
      onClick: (event) => void check(event.currentTarget as HTMLButtonElement),
    });
    checkButton.id = "about-check";

    actions.append(
      checkButton,
      button({
        label: ctx.i18n.t("about.changelog"),
        variant: "secondary",
        iconRight: "external",
        onClick: openChangelog,
      }),
    );

    card.append(mark, copy, actions);
    return card;
  };

  const renderInfo = (): HTMLElement => {
    const grid = el("div", "about__grid");

    const license = panel({
      title: ctx.i18n.t("about.license"),
      icon: "shield",
      className: "about__card",
    });
    const licenseBody = el("div", "about__row");
    licenseBody.append(
      el("span", "about__value", ctx.i18n.t("about.licenseValue")),
      el("span", "about__hint", ctx.i18n.t("about.licenseDesc")),
    );
    license.body.append(licenseBody);

    const repo = panel({
      title: ctx.i18n.t("about.repo"),
      icon: "external",
      className: "about__card",
    });
    const repoLink = document.createElement("a");
    repoLink.className = "about__link";
    repoLink.href = RepositoryUrl;
    repoLink.target = "_blank";
    repoLink.rel = "noreferrer noopener";
    repoLink.append(
      el("span", "about__value", ctx.i18n.t("about.repoValue")),
      el("span", "about__hint", ctx.i18n.t("about.repoHint")),
    );
    const repoGlyph = el("span", "about__linkIcon");
    repoGlyph.innerHTML = icon("external", 14);
    repoLink.append(repoGlyph);
    repo.body.append(repoLink);

    const stack = panel({
      title: ctx.i18n.t("about.stack"),
      icon: "sparkles",
      className: "about__card",
    });
    const stackBody = el("div", "about__row");
    stackBody.append(
      el("span", "about__value", ctx.i18n.t("about.stackValue")),
      el("span", "about__hint", ctx.i18n.t("about.stackDesc")),
    );
    stack.body.append(stackBody);

    grid.append(license.el, repo.el, stack.el);
    return grid;
  };

  const renderFooter = (): HTMLElement => {
    const footer = el("footer", "about__footer");
    footer.append(
      el("span", "about__copyright", ctx.i18n.t("about.copyright")),
      el("span", "about__copyright", ctx.i18n.t("about.madeWith")),
    );
    return footer;
  };

  const render = (): void => {
    root.replaceChildren(renderHeader(), renderHero(), renderInfo(), renderFooter());
  };

  render();

  return { el: root, render };
}
