import { useQueries } from '@tanstack/react-query';
import { api } from '@/lib/api';
import {
  getProviderHealthQueryKey,
  isProviderConfigComplete,
  type ProviderConfigResponse,
  type ProviderHealthById,
  type ProviderHealthSnapshot,
} from '@/lib/provider-types';

export function useProviderHealthById(
  providers: ProviderConfigResponse[] | undefined,
  options: { autoTest?: boolean } = {},
): ProviderHealthById {
  const results = useQueries({
    queries: (providers ?? []).map((provider) => ({
      queryKey: getProviderHealthQueryKey(provider.id),
      queryFn: () => api.testProvider(provider.id),
      enabled: Boolean(options.autoTest && isProviderConfigComplete(provider)),
      staleTime: Infinity,
      retry: false,
    })),
  });

  return (providers ?? []).reduce<ProviderHealthById>((acc, provider, index) => {
    const data = results[index]?.data as ProviderHealthSnapshot | undefined;
    if (data) acc[provider.id] = data;
    return acc;
  }, {});
}
