/** Russian/English plural forms for the counts the dashboard shows. */

type RussianForms = readonly [one: string, few: string, many: string];
type EnglishForms = readonly [one: string, many: string];

/** Russian nouns take the singular for 1 and 21, the few-form for 2..4 and 22..24, else many. */
export function russianForm(count: number): 0 | 1 | 2 {
  const value = Math.abs(count) % 100;
  if (value >= 11 && value <= 14) {
    return 2;
  }

  switch (value % 10) {
    case 1:
      return 0;
    case 2:
    case 3:
    case 4:
      return 1;
    default:
      return 2;
  }
}

/** The noun for `count`: «1 мод», «2 мода», «5 модов». */
export function plural(language: string, count: number, ru: RussianForms, en: EnglishForms): string {
  if (language === "en") {
    return count === 1 ? en[0] : en[1];
  }

  return ru[russianForm(count)];
}

/** The formatted count together with its noun: «2 мода», «12 строк». */
export function countLabel(
  language: string,
  count: number,
  ru: RussianForms,
  en: EnglishForms,
): string {
  const number = new Intl.NumberFormat(language === "en" ? "en-US" : "ru-RU").format(count);
  return `${number} ${plural(language, count, ru, en)}`;
}

export const nouns = {
  mods: {
    ru: ["мод", "мода", "модов"] as const,
    en: ["mod", "mods"] as const,
  },
  profiles: {
    ru: ["профиль", "профиля", "профилей"] as const,
    en: ["profile", "profiles"] as const,
  },
  presets: {
    ru: ["профиль", "профиля", "профилей"] as const,
    en: ["preset", "presets"] as const,
  },
  rules: {
    ru: ["правило", "правила", "правил"] as const,
    en: ["rule", "rules"] as const,
  },
  widgets: {
    ru: ["виджет", "виджета", "виджетов"] as const,
    en: ["widget", "widgets"] as const,
  },
  files: {
    ru: ["файл", "файла", "файлов"] as const,
    en: ["file", "files"] as const,
  },
  lines: {
    ru: ["строка", "строки", "строк"] as const,
    en: ["line", "lines"] as const,
  },
  seconds: {
    ru: ["секунда", "секунды", "секунд"] as const,
    en: ["second", "seconds"] as const,
  },
  minutes: {
    ru: ["минута", "минуты", "минут"] as const,
    en: ["minute", "minutes"] as const,
  },
} as const;
