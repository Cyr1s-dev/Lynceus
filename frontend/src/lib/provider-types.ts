export type ProviderType =
  | 'openai'
  | 'anthropic'
  | 'gemini'
  | 'openai_compatible'
  | 'ollama'
  | 'vllm'
  | 'lm_studio'
  | 'llama_cpp'
  | 'local'
  | 'codex_cli'
  | 'claude_code'
  | 'mcp_remote'
  | 'custom';

export type McpTransport = 'sse' | 'streamable_http' | 'stdio_placeholder';

export interface ProviderConfigResponse {
  id: string;
  name: string;
  provider_type: ProviderType;
  base_url?: string | null;
  model?: string | null;
  api_key_ref?: string | null;
  has_api_key: boolean;
  default_headers: Record<string, string>;
  timeout_seconds: number;
  max_tokens?: number | null;
  temperature?: number | null;
  is_default: boolean;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface CreateProviderRequest {
  name: string;
  provider_type: ProviderType;
  base_url?: string | null;
  model?: string | null;
  api_key?: string | null;
  api_key_ref?: string | null;
  default_headers?: Record<string, string>;
  timeout_seconds?: number;
  max_tokens?: number | null;
  temperature?: number | null;
  is_default?: boolean;
  enabled?: boolean;
}

export type UpdateProviderRequest = Partial<CreateProviderRequest>;

export interface DiscoverProviderModelsRequest {
  provider_id?: string | null;
  provider_type?: ProviderType | null;
  base_url?: string | null;
  model?: string | null;
  api_key?: string | null;
  api_key_ref?: string | null;
  default_headers?: Record<string, string> | null;
  timeout_seconds?: number | null;
}

export interface ProviderModelDiscoveryResult {
  provider_id?: string | null;
  status: 'ok' | 'error' | 'timeout' | 'denied';
  message: string;
  endpoint: string;
  models: string[];
  configured_model?: string | null;
  configured_model_available?: boolean | null;
}

export interface ProviderHealthResult {
  provider_id: string;
  status: 'ok' | 'error' | 'timeout' | 'denied';
  message: string;
  model?: string | null;
  model_invocation_id?: string | null;
  capabilities?: {
    text_generation?: boolean;
    structured_output?: boolean;
    [key: string]: boolean | undefined;
  };
}

export const DEFAULT_PROVIDER_VALUE = '__default__';

export type ProviderCapabilityRequirement = 'text_generation' | 'structured_output';

export type ProviderHealthSnapshot =
  Omit<ProviderHealthResult, 'status'> & {
    status: ProviderHealthResult['status'] | 'testing' | 'not_tested';
  };

export type ProviderHealthById = Record<string, ProviderHealthSnapshot | undefined>;

export type ProviderReadinessKind =
  | 'disabled'
  | 'unsupported'
  | 'incomplete'
  | 'untested'
  | 'testing'
  | 'denied'
  | 'timeout'
  | 'error'
  | 'no_text_generation'
  | 'text_only'
  | 'ready';

export interface ProviderReadiness {
  kind: ProviderReadinessKind;
  labelKey: string;
  configComplete: boolean;
  healthKnown: boolean;
  healthOk: boolean;
  textGeneration: boolean;
  structuredOutput: boolean;
  usableForTextGeneration: boolean;
  usableForStructuredOutput: boolean;
}

export function getProviderHealthQueryKey(providerId: string) {
  return ['providers', 'health', providerId] as const;
}

export const RUNTIME_SUPPORTED_PROVIDER_TYPES = new Set<ProviderType>([
  'openai',
  'anthropic',
  'gemini',
  'openai_compatible',
  'ollama',
  'vllm',
  'lm_studio',
  'llama_cpp',
  'local',
  'custom',
]);

const API_KEY_REQUIRED_PROVIDER_TYPES = new Set<ProviderType>([
  'openai',
  'anthropic',
  'gemini',
  'openai_compatible',
  'custom',
]);

const BASE_URL_REQUIRED_PROVIDER_TYPES = new Set<ProviderType>([
  'vllm',
  'llama_cpp',
  'local',
  'custom',
]);

export function isProviderRuntimeSupported(providerType: ProviderType): boolean {
  return RUNTIME_SUPPORTED_PROVIDER_TYPES.has(providerType);
}

export function providerRequiresApiKey(providerType: ProviderType): boolean {
  return API_KEY_REQUIRED_PROVIDER_TYPES.has(providerType);
}

export function providerRequiresModel(providerType: ProviderType): boolean {
  return RUNTIME_SUPPORTED_PROVIDER_TYPES.has(providerType);
}

export function providerRequiresBaseUrl(providerType: ProviderType): boolean {
  return BASE_URL_REQUIRED_PROVIDER_TYPES.has(providerType);
}

export function isProviderConfigComplete(provider: ProviderConfigResponse): boolean {
  if (!provider.enabled) return false;
  if (!isProviderRuntimeSupported(provider.provider_type)) return false;
  if (providerRequiresApiKey(provider.provider_type) && !provider.has_api_key) return false;
  if (providerRequiresModel(provider.provider_type) && !provider.model?.trim()) return false;
  if (providerRequiresBaseUrl(provider.provider_type) && !provider.base_url?.trim()) return false;
  return true;
}

export function isProviderUsable(
  provider: ProviderConfigResponse,
  health: ProviderHealthSnapshot | undefined,
  requirement: ProviderCapabilityRequirement = 'text_generation'
): boolean {
  const readiness = getProviderReadiness(provider, health);
  return requirement === 'structured_output'
    ? readiness.usableForStructuredOutput
    : readiness.usableForTextGeneration;
}

export function isProviderHealthUsable(health: ProviderHealthSnapshot | undefined): boolean | null {
  if (!health) return null;
  if (health.status !== 'ok') return false;
  return Boolean(health.capabilities?.text_generation);
}

export function getProviderReadiness(
  provider: ProviderConfigResponse,
  health: ProviderHealthSnapshot | undefined
): ProviderReadiness {
  const base: Omit<ProviderReadiness, 'kind' | 'labelKey'> = {
    configComplete: false,
    healthKnown: Boolean(health && health.status !== 'not_tested'),
    healthOk: health?.status === 'ok',
    textGeneration: health?.capabilities?.text_generation === true,
    structuredOutput: health?.capabilities?.structured_output === true,
    usableForTextGeneration: false,
    usableForStructuredOutput: false,
  };

  const finish = (
    kind: ProviderReadinessKind,
    overrides: Partial<Omit<ProviderReadiness, 'kind' | 'labelKey'>> = {}
  ): ProviderReadiness => {
    const merged = { ...base, ...overrides };
    return {
      kind,
      labelKey: `providerReadiness.${kind}`,
      ...merged,
    };
  };

  if (!provider.enabled) return finish('disabled');
  if (!isProviderRuntimeSupported(provider.provider_type)) return finish('unsupported');
  if (!isProviderConfigComplete(provider)) return finish('incomplete');

  const configured = { configComplete: true };
  if (!health || health.status === 'not_tested') {
    return finish('untested', configured);
  }
  if (health.status === 'testing') return finish('testing', configured);
  if (health.status === 'denied') return finish('denied', configured);
  if (health.status === 'timeout') return finish('timeout', configured);
  if (health.status === 'error') return finish('error', configured);

  if (health.capabilities?.text_generation !== true) {
    return finish('no_text_generation', { ...configured, healthOk: true });
  }
  if (health.capabilities.structured_output !== true) {
    return finish('text_only', {
      ...configured,
      healthOk: true,
      textGeneration: true,
      usableForTextGeneration: true,
    });
  }
  return finish('ready', {
    ...configured,
    healthOk: true,
    textGeneration: true,
    structuredOutput: true,
    usableForTextGeneration: true,
    usableForStructuredOutput: true,
  });
}

export function getUsableProviders(
  providers: ProviderConfigResponse[] | undefined,
  healthById: ProviderHealthById,
  requirement: ProviderCapabilityRequirement = 'text_generation'
): ProviderConfigResponse[] {
  return (providers ?? []).filter(provider =>
    isProviderUsable(provider, healthById[provider.id], requirement)
  );
}

export function getProviderReadinessBadgeClassName(kind: ProviderReadinessKind): string {
  switch (kind) {
    case 'ready':
      return 'bg-green-50 text-green-700 border-green-200';
    case 'text_only':
    case 'untested':
    case 'testing':
      return 'bg-amber-50 text-amber-700 border-amber-200';
    case 'disabled':
    case 'unsupported':
    case 'incomplete':
      return 'bg-slate-50 text-slate-500 border-slate-200';
    case 'denied':
      return 'bg-blue-50 text-blue-700 border-blue-200';
    case 'timeout':
      return 'bg-orange-50 text-orange-700 border-orange-200';
    case 'error':
    case 'no_text_generation':
      return 'bg-red-50 text-red-700 border-red-200';
    default:
      return 'bg-slate-50 text-slate-500 border-slate-200';
  }
}

export const PROVIDER_METADATA: Record<ProviderType, {
  label: string;
  icon?: string;
  defaultBaseUrl?: string;
  defaultModel?: string;
  suggestedModels?: string[];
  description?: string;
}> = {
  openai: {
    label: 'OpenAI',
    defaultBaseUrl: 'https://api.openai.com/v1',
    defaultModel: 'gpt-5.5',
    suggestedModels: ['gpt-5.5', 'gpt-5.5-pro', 'gpt-5.4', 'gpt-5.4-pro', 'gpt-5.4-mini', 'gpt-5.4-nano'],
  },
  openai_compatible: {
    label: 'OpenAI Compatible',
    defaultBaseUrl: 'https://api.openai.com/v1',
    defaultModel: 'gpt-5.5',
    // 兼容端点的建议只给少量代表；真实列表用对话框里的"获取模型"拉取。
    suggestedModels: [
      'gpt-5.5',
      'claude-opus-4-8',
      'gemini-3.1-pro-preview',
      'deepseek-chat',
      'qwen3-coder',
    ],
  },
  anthropic: {
    label: 'Anthropic Claude',
    defaultBaseUrl: 'https://api.anthropic.com/v1',
    defaultModel: 'claude-opus-4-8',
    suggestedModels: ['claude-opus-4-8', 'claude-fable-5', 'claude-sonnet-4-6', 'claude-haiku-4-5'],
  },
  gemini: {
    label: 'Google Gemini',
    defaultBaseUrl: 'https://generativelanguage.googleapis.com/v1beta',
    defaultModel: 'gemini-3.1-pro-preview',
    suggestedModels: [
      'gemini-3.1-pro-preview',
      'gemini-3.1-pro-preview-customtools',
      'gemini-3.5-flash',
      'gemini-3.1-flash-lite',
      'gemini-3-flash-preview',
    ],
  },
  ollama: {
    label: 'Ollama',
    defaultBaseUrl: 'http://127.0.0.1:11434/v1',
    defaultModel: 'qwen3:latest',
    suggestedModels: ['qwen3:latest', 'qwen3-coder:latest', 'qwen2.5-coder:latest', 'llama3.3:latest'],
  },
  lm_studio: {
    label: 'LM Studio',
    defaultBaseUrl: 'http://127.0.0.1:1234/v1',
    defaultModel: 'local-model',
    suggestedModels: ['local-model'],
  },
  vllm: {
    label: 'vLLM',
    defaultBaseUrl: 'http://127.0.0.1:8001/v1',
    defaultModel: 'qwen3',
    suggestedModels: ['qwen3', 'qwen3-coder', 'deepseek-coder', 'llama'],
  },
  llama_cpp: {
    label: 'llama.cpp',
    defaultBaseUrl: 'http://127.0.0.1:8080/v1',
    defaultModel: 'local-model',
    suggestedModels: ['local-model'],
  },
  local: {
    label: 'Local OpenAI-compatible',
    defaultBaseUrl: 'http://127.0.0.1:8001/v1',
    defaultModel: 'local-model',
    suggestedModels: ['local-model'],
  },
  codex_cli: { label: 'Codex CLI' },
  claude_code: { label: 'Claude Code' },
  mcp_remote: { label: 'MCP Remote' },
  custom: {
    label: 'Custom HTTP Provider',
    defaultModel: 'custom-model',
    suggestedModels: ['custom-model'],
  },
};

/**
 * Worker Runtime（外部 Harness）wire 值。
 *
 * 独立于 `types.ts` 的 `WorkerRuntimeType` 声明，避免 lib 内部循环依赖。
 */
export type WorkerRuntimeTypeValue =
  | 'claude_code'
  | 'codex'
  | 'pi'
  | 'deepseek_harness';

/**
 * Worker Runtime（外部 Harness）展示名。
 *
 * 与后端 `WorkerRuntimeType::all()` 的稳定顺序一致。所有需要显示
 * "这次是哪个 Harness 在干活"的界面（Mission 会话面板、Worker Runtimes
 * 页）都必须用这一份，避免两处映射漂移后同一 runtime 在两个面板上显示成
 * 不同名字。
 */
export const WORKER_HARNESS_NAMES: Record<WorkerRuntimeTypeValue, string> = {
  claude_code: 'Claude Code',
  codex: 'Codex',
  pi: 'Pi',
  deepseek_harness: 'DSH',
};

/** 取 Harness 展示名；未知 runtime 原样返回（不静默吞掉新 runtime）。 */
export function workerHarnessName(runtime?: string | null): string | null {
  if (!runtime) return null;
  return WORKER_HARNESS_NAMES[runtime as WorkerRuntimeTypeValue] ?? runtime;
}
