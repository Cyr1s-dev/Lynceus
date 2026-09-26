import type { TFunction } from 'i18next';

function formatKeyedValue(t: TFunction, prefix: string, value?: string | null): string {
  if (!value) return t('common.unknown');
  return t(`${prefix}.${value}`, value);
}

export function formatStatus(t: TFunction, status?: string | null): string {
  if (!status) return t('common.unknown');
  const key = status.replaceAll(' ', '_');
  return formatKeyedValue(t, 'status', key);
}

export function formatFindingStatus(t: TFunction, status?: string | null): string {
  return formatKeyedValue(t, 'findingStatus', status);
}

export function formatSeverity(t: TFunction, severity?: string | null): string {
  return formatKeyedValue(t, 'severity', severity);
}

export function formatModuleDomain(t: TFunction, domain?: string | null): string {
  return formatKeyedValue(t, 'modules.domain', domain);
}

export function formatModuleTransport(t: TFunction, transport?: string | null): string {
  return formatKeyedValue(t, 'modules.transport', transport);
}

export function formatModuleProfile(t: TFunction, profile?: string | null): string {
  return formatKeyedValue(t, 'modules.profile', profile);
}

export function formatModuleType(t: TFunction, moduleType?: string | null): string {
  return formatKeyedValue(t, 'modules.moduleType', moduleType);
}
