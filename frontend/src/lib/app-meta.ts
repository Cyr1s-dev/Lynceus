/**
 * Product metadata shown in chrome (sidebar footer, about surfaces).
 * Keep this as the single source of truth for UI-facing version strings.
 */
export const APP_NAME = 'Lynceus'
export const APP_VERSION = '0.1.0'
export const APP_BUILD_CHANNEL = (
  import.meta.env.VITE_APP_BUILD_CHANNEL?.trim() || 'dev'
).toLowerCase()

/** e.g. `v0.1.0` or `v0.1.0-dev` */
export function formatAppVersion(includeChannel = true): string {
  if (includeChannel && APP_BUILD_CHANNEL && APP_BUILD_CHANNEL !== 'stable') {
    return `v${APP_VERSION}-${APP_BUILD_CHANNEL}`
  }
  return `v${APP_VERSION}`
}
