import { en } from "./en";
import { ru, type MessageKey, type Messages } from "./ru";

export type { MessageKey } from "./ru";

/** Languages the dashboard ships. Anything unknown falls back to Russian. */
export type Language = "ru" | "en";

export const Languages: readonly Language[] = ["ru", "en"];

const tables: Record<Language, Messages> = { ru, en };

export type TParams = Record<string, string | number>;

export interface I18n {
  readonly language: Language;
  /** Looks `key` up in the active table; an unknown key returns the key itself. */
  t(key: MessageKey, params?: TParams): string;
  setLanguage(language: string | null | undefined): boolean;
  onChange(handler: (language: Language) => void): void;
}

/** <c>ru-RU</c>, <c>en-US</c> and friends; anything not English is Russian. */
export function normalizeLanguage(value: string | null | undefined): Language {
  return typeof value === "string" && value.toLowerCase().startsWith("en") ? "en" : "ru";
}

/** The language the host or browser asked for, before the config is read. */
export function browserLanguage(): Language {
  return normalizeLanguage(typeof navigator === "undefined" ? null : navigator.language);
}

export function createI18n(initial: Language): I18n {
  let language: Language = initial;
  const listeners = new Set<(language: Language) => void>();

  return {
    get language() {
      return language;
    },

    t(key, params) {
      const template = tables[language][key] ?? key;
      if (!params) {
        return template;
      }

      return template.replace(/\{(\w+)\}/g, (match, name: string) => {
        const value = params[name];
        return value === undefined ? match : String(value);
      });
    },

    setLanguage(next) {
      const normalized = normalizeLanguage(next);
      if (normalized === language) {
        return false;
      }

      language = normalized;
      for (const listener of listeners) {
        listener(language);
      }

      return true;
    },

    onChange(handler) {
      listeners.add(handler);
    },
  };
}
