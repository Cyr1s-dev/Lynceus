import { useCallback, useMemo } from 'react';
import { useTranslation } from 'react-i18next';

import { normalizeLocale, persistLocale, type SupportedLocale } from '@/i18n';

const LOCALE_LABELS: Record<SupportedLocale, string> = {
  en: 'EN',
  'zh-CN': '中文',
};

export function useLocale() {
  const { i18n } = useTranslation();
  const locale = useMemo<SupportedLocale>(
    () => normalizeLocale(i18n.resolvedLanguage || i18n.language) ?? 'en',
    [i18n.language, i18n.resolvedLanguage]
  );
  const nextLocale: SupportedLocale = locale === 'zh-CN' ? 'en' : 'zh-CN';

  const setLocale = useCallback(
    (targetLocale: SupportedLocale) => {
      persistLocale(targetLocale);
      void i18n.changeLanguage(targetLocale);
    },
    [i18n]
  );

  const toggleLocale = useCallback(() => {
    setLocale(nextLocale);
  }, [nextLocale, setLocale]);

  return {
    locale,
    label: LOCALE_LABELS[locale],
    nextLocale,
    nextLabel: LOCALE_LABELS[nextLocale],
    setLocale,
    toggleLocale,
  };
}
