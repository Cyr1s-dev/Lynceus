const STORAGE_KEYS = {
  API_BASE_URL: 'lynceus_api_base_url',
  PROVIDERS: 'lynceus_agent_providers', // Legacy key
};

export const getApiBaseUrl = (): string => {
  const stored = localStorage.getItem(STORAGE_KEYS.API_BASE_URL);
  if (stored) return stored;
  return import.meta.env.VITE_API_BASE_URL || 'http://127.0.0.1:8000';
};

export const setApiBaseUrl = (url: string) => {
  localStorage.setItem(STORAGE_KEYS.API_BASE_URL, url);
};

export const clearLegacyLocalState = () => {
  localStorage.removeItem(STORAGE_KEYS.PROVIDERS);
};
