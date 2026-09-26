import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';

import en from './resources/en';
import zhCN from './resources/zh-CN';

export const SUPPORTED_LOCALES = ['en', 'zh-CN'] as const;
export type SupportedLocale = (typeof SUPPORTED_LOCALES)[number];

const LOCALE_STORAGE_KEY = 'lynceus_locale';

const resources = {
  en: {
    translation: en,
  },
  'zh-CN': {
    translation: zhCN,
  },
};

export function isSupportedLocale(locale: unknown): locale is SupportedLocale {
  return typeof locale === 'string' && SUPPORTED_LOCALES.includes(locale as SupportedLocale);
}

export function normalizeLocale(locale?: string | null): SupportedLocale | null {
  if (!locale) return null;
  if (isSupportedLocale(locale)) return locale;

  const normalized = locale.toLowerCase();
  if (normalized === 'zh' || normalized === 'zh-cn' || normalized === 'zh-hans' || normalized.startsWith('zh-hans-')) {
    return 'zh-CN';
  }
  if (normalized === 'en' || normalized.startsWith('en-')) {
    return 'en';
  }

  return null;
}

export function getSavedLocale(): SupportedLocale | null {
  try {
    if (typeof window === 'undefined' || !window.localStorage) return null;
    const savedLocale = window.localStorage.getItem(LOCALE_STORAGE_KEY);
    return isSupportedLocale(savedLocale) ? savedLocale : null;
  } catch {
    return null;
  }
}

export function getBrowserLocale(): SupportedLocale {
  if (typeof navigator === 'undefined') return 'en';

  const candidates = [
    ...(Array.isArray(navigator.languages) ? navigator.languages : []),
    navigator.language,
  ];

  for (const candidate of candidates) {
    const locale = normalizeLocale(candidate);
    if (locale) return locale;
  }

  return 'en';
}

export function resolveInitialLocale(): SupportedLocale {
  return getSavedLocale() ?? getBrowserLocale();
}

export function persistLocale(locale: SupportedLocale): void {
  if (!isSupportedLocale(locale)) return;

  try {
    if (typeof window === 'undefined' || !window.localStorage) return;
    window.localStorage.setItem(LOCALE_STORAGE_KEY, locale);
  } catch {
    // Storage may be unavailable in SSR, Storybook, private mode, or tests.
  }
}

i18n
  .use(initReactI18next)
  .init({
    resources,
    lng: resolveInitialLocale(),
    fallbackLng: 'en',
    supportedLngs: [...SUPPORTED_LOCALES],
    interpolation: {
      escapeValue: false, // react already safes from xss
    },
  });

export default i18n;
