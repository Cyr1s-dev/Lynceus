import axios from 'axios';
import { getApiBaseUrl } from './settings';
import {
  asView,
  contractPath,
  contractGet,
  contractPost,
  contractPut,
  contractPatch,
  contractDelete,
  type ContractQuery,
  type ContractRequestBody,
} from './contract';
import type {
  AgentPreset,
  AgentPresetPreviewResponse,
  CreateAgentPresetRequest,
  CreateSkillRequest,
  ExplorationGraph,
  GraphSnapshot,
  SkillMeta,
  SkillMissingEntry,
  SkillUsageRow,
  DashboardStats,
  GatewayStatus,
  UpdateAgentPresetRequest,
  WorkerUsageBreakdown,
  WorkerUsageDimension,
  WorkerProbe,
  WorkerRun,
  WorkerRuntimeProfile,
  UpsertWorkerRuntimeProfileRequest,
  ApiFinding,
  FindingAssetTree,
  WorkerCommandPolicy,
  ApiAuditRun,
  ApiProject,
  ApiToolInvocation,
  ApiAuditEvent,
  CoverageGraph,
  FindingRetest,
  FindingStatus,
  ReportEnvelope,
  Severity,
  ApiModule,
  CreateModuleRequest,
  UpdateModuleRequest,
  ModuleHealthResult,
  DecisionGate,
  DecisionAnswer,
  AuditDomain,
  Observation,
  ReflectorReport,
  TerminationAssessment,
  WorkerLease,
  StrategyBoardSnapshot,
  KnowledgeCard,
  KnowledgeCorpusStatus,
  KnowledgeRetrievalQuery,
  KnowledgeRetrievalResult,
  ArtifactRecord,
  UploadArtifactResponse,
  MissionAsset,
  RuntimeSetting,
  ProviderRouteBinding,
  IntakeAnalyzeRequest,
  IntakeAnalyzeResponse,
  IntakeStartRequest,
  IntakeStartResponse,
  Mission,
  MissionCanvas,
  MissionStartResponse,
  ApprovalMode,
  SwarmOperationPage,
  UserDirective,
  UserDirectiveType,
  Branch,
  ToolCatalogEntry,
  ToolDetectionStatus,
  ToolInstallJob,
  ToolRecommendation,
  AgentNarrativeEvent,
  CreateAgentNarrativeRequest,
  EnginePoolEntry,
  MissionEvidenceSummary,
  MissionSignalRequest,
  MissionSignalResponse,
  IntelQuery,
  IntelEntityKind,
  IntelEntityRecord,
  IntelRelationRecord,
  IntelSourceInfo,
  IntelExpansionReport,
  IntelRawRecord,
  IntelPromoteOutcome,
} from './types';
import type {
  ProviderConfigResponse,
  CreateProviderRequest,
  UpdateProviderRequest,
  DiscoverProviderModelsRequest,
  ProviderHealthResult,
  ProviderModelDiscoveryResult,
} from './provider-types';

export { apiClient } from './contract';

const MODEL_OPERATION_TIMEOUT_MS = 120_000;

/**
 * 顾问/复测这类"起一个只读 CLI worker 回答问题"的操作超时。
 *
 * 服务端 `WorkerExecutionRequest::start(instruction, 240)` 给 advisor 240s，
 * 而 `apiClient` 默认只有 30s（CONTROL_PLANE_TIMEOUT_MS）——实测一次正常
 *  advise 就要 ~70s，前端必然在服务端算完之前掐断，界面表现为"旁路提问
 * 用不了"。这里必须显著大于服务端上限，否则请求永远等不到答案。
 */
const ADVISOR_OPERATION_TIMEOUT_MS = 260_000;

export function getApiErrorMessage(error: unknown): string {
  if (axios.isAxiosError(error)) {
    if (error.response?.data?.detail) {
      if (typeof error.response.data.detail === 'string') {
        return error.response.data.detail;
      }
      return JSON.stringify(error.response.data.detail);
    }
    const timedOut =
      error.code === 'ECONNABORTED'
      || error.code === 'ETIMEDOUT'
      || error.message.toLowerCase().includes('timeout');
    if (timedOut) {
      const configuredTimeout = error.config?.timeout;
      const seconds =
        typeof configuredTimeout === 'number'
          ? Math.max(1, Math.round(configuredTimeout / 1000))
          : null;
      return `Request timed out${seconds ? ` after ${seconds}s` : ''}. The operation may still be running; refresh the task status before retrying.`;
    }
    if (error.message === 'Network Error' || !error.response) {
      return 'Network Error: check API Base URL, backend status, and CORS settings.';
    }
    return error.message;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return 'Unknown error';
}

export const api = {
  checkHealth: async (customBaseUrl?: string): Promise<boolean> => {
    const url = customBaseUrl || getApiBaseUrl();
    try {
      await axios.get(`${url.replace(/\/$/, '')}${contractPath('/health', {})}`, { timeout: 2000 });
      return true;
    } catch {
      try {
        await axios.get(url, { timeout: 2000 });
        return true;
      } catch {
        return false;
      }
    }
  },

  getProjectEvents: async (
    projectId: string,
    params?: { run_id?: string; limit?: number; after_id?: string }
  ): Promise<ApiAuditEvent[]> => {
    return asView<ApiAuditEvent[]>(await contractGet('/projects/{project_id}/events', {
      path: { project_id: projectId },
      query: params,
    }));
  },

  getProjects: async (): Promise<ApiProject[]> => {
    return asView<ApiProject[]>(await contractGet('/projects'));
  },

  getMissions: async (projectId?: string): Promise<Mission[]> => {
    return asView<Mission[]>(await contractGet('/missions', {
      query: projectId ? { project_id: projectId } : undefined,
    }));
  },

  getMission: async (id: string): Promise<Mission> => {
    return asView<Mission>(await contractGet('/missions/{mission_id}', { path: { mission_id: id } }));
  },

  reassessMissionCompletion: async (missionId: string): Promise<TerminationAssessment> => {
    return contractPost('/missions/{mission_id}/reassess', {
      path: { mission_id: missionId },
      config: { timeout: MODEL_OPERATION_TIMEOUT_MS },
    });
  },

  createMission: async (input: {
    user_goal: string;
    title?: string | null;
    target?: Record<string, string>;
    project_id?: string | null;
    constraints?: string[];
    success_criteria?: string[];
    goal_contract?: Mission['goal_contract'];
    tags?: string[];
    category?: string | null;
    approval_mode?: ApprovalMode;
    created_by?: string;
    metadata?: Record<string, unknown>;
  }): Promise<Mission> => {
    return asView<Mission>(await contractPost('/missions', {
      data: input as ContractRequestBody<'/missions', 'post'>,
    }));
  },

  updateMission: async (
    missionId: string,
    input: {
      user_goal?: string;
      title?: string | null;
      tags?: string[];
      category?: string | null;
      archived?: boolean;
      approval_mode?: ApprovalMode;
      metadata?: Record<string, unknown>;
      goal_contract?: Mission['goal_contract'];
    }
  ): Promise<Mission> => {
    return asView<Mission>(await contractPatch('/missions/{mission_id}', {
      path: { mission_id: missionId },
      data: input as ContractRequestBody<'/missions/{mission_id}', 'patch'>,
    }));
  },

  deleteMission: async (missionId: string): Promise<void> => {
    await contractDelete('/missions/{mission_id}', { path: { mission_id: missionId } });
  },

  batchMissionAction: async (
    input: {
      mission_ids: string[];
      action: 'archive' | 'restore' | 'delete' | 'set_category' | 'add_tags' | 'remove_tags';
      tags?: string[];
      category?: string | null;
    }
  ): Promise<Mission[]> => {
    return asView<Mission[]>(await contractPost('/missions/batch', { data: input }));
  },

  startMission: async (
    missionId: string,
    input: { config?: Record<string, unknown>; auto_start_runtime?: boolean; max_concurrent_branches?: number; max_total_steps?: number } = {}
  ): Promise<MissionStartResponse> => {
    return asView<MissionStartResponse>(await contractPost('/missions/{mission_id}/start', {
      path: { mission_id: missionId },
      data: input,
    }));
  },

  pauseMission: async (missionId: string, reason?: string): Promise<Mission> => {
    return asView<Mission>(await contractPost('/missions/{mission_id}/pause', {
      path: { mission_id: missionId },
      data: { reason: reason || 'Paused from Console' },
    }));
  },

  resumeMission: async (
    missionId: string,
    input: { new_requirement?: string | null; config?: Record<string, unknown> } = {}
  ): Promise<MissionStartResponse> => {
    return asView<MissionStartResponse>(await contractPost('/missions/{mission_id}/resume', {
      path: { mission_id: missionId },
      data: input,
    }));
  },

  getMissionBranches: async (missionId: string): Promise<Branch[]> => {
    return contractGet('/missions/{mission_id}/branches', { path: { mission_id: missionId } });
  },

  /* ── 漏洞 triage / 覆盖图 / 报告 / 复测（后端能力对齐） ── */

  /** 人工 triage：改写 Finding 状态/严重度（至少给一个字段）。 */
  triageFinding: async (
    projectId: string,
    findingId: string,
    body: { status?: FindingStatus; severity?: Severity },
  ): Promise<ApiFinding> => {
    return asView<ApiFinding>(
      await contractPatch('/projects/{project_id}/findings/{finding_id}', {
        path: { project_id: projectId, finding_id: findingId },
        data: body,
      }),
    );
  },

  /** 资产覆盖图（后端推导的层级树 + 已测/未测）。 */
  missionCoverageGraph: async (missionId: string): Promise<CoverageGraph> => {
    return asView<CoverageGraph>(
      await contractGet('/missions/{mission_id}/coverage-graph', {
        path: { mission_id: missionId },
      }),
    );
  },

  /** 任务报告。`reportId` 可以是 Mission / AuditRun / Project id。 */
  getReport: async (
    reportId: string,
    params: { format?: 'json' | 'markdown'; include_statuses?: string; download?: boolean } = {},
  ): Promise<ReportEnvelope> => {
    return asView<ReportEnvelope>(
      await contractGet('/reports/{report_id}', {
        path: { report_id: reportId },
        query: params,
      }),
    );
  },

  /** 发起漏洞复测（同步等待顾问 worker 结论后返回记录）。 */  startFindingRetest: async (
    missionId: string,
    findingId: string,
    notes: string,
  ): Promise<FindingRetest> => {
    return asView<FindingRetest>(
      await contractPost('/missions/{mission_id}/findings/{finding_id}/retests', {
        path: { mission_id: missionId, finding_id: findingId },
        data: { notes },
        config: { timeout: ADVISOR_OPERATION_TIMEOUT_MS },
      }),
    );
  },

  /** 列某个漏洞的全部复测记录（新的在前）。 */
  listFindingRetests: async (
    missionId: string,
    findingId: string,
  ): Promise<FindingRetest[]> => {
    return asView<FindingRetest[]>(
      await contractGet('/missions/{mission_id}/findings/{finding_id}/retests', {
        path: { mission_id: missionId, finding_id: findingId },
      }),
    );
  },

  /** 列某任务的未收口复测（"复测中"角标）。 */
  listActiveMissionRetests: async (missionId: string): Promise<FindingRetest[]> => {
    return asView<FindingRetest[]>(
      await contractGet('/missions/{mission_id}/retests/active', {
        path: { mission_id: missionId },
      }),
    );
  },

  getMissionOperationLog: async (    missionId: string,
    params: {
      after_sequence?: number;
      limit?: number;
      run_id?: string;
      branch_id?: string;
      task_id?: string;
      worker_id?: string;
      operation_type?: string;
    } = {},
  ): Promise<SwarmOperationPage> => {
    return contractGet('/missions/{mission_id}/operation-log', {
      path: { mission_id: missionId },
      query: params,
    });
  },

  prioritizeBranch: async (branchId: string, reason?: string): Promise<UserDirective> => {
    return contractPost('/branches/{branch_id}/prioritize', {
      path: { branch_id: branchId },
      data: { reason: reason || 'Prioritized from Console' },
    });
  },

  abandonBranch: async (branchId: string, reason?: string): Promise<Branch> => {
    return contractPost('/branches/{branch_id}/abandon', {
      path: { branch_id: branchId },
      data: { reason: reason || 'Abandoned from Console' },
    });
  },

  reopenBranch: async (branchId: string, reason?: string): Promise<Branch> => {
    return contractPost('/branches/{branch_id}/reopen', {
      path: { branch_id: branchId },
      data: { reason: reason || 'Reopened from Console' },
    });
  },

  applyMissionDirective: async (
    missionId: string,
    input: { directive_type: UserDirectiveType; content: string; branch_id?: string | null; metadata?: Record<string, unknown> }
  ): Promise<UserDirective> => {
    return contractPost('/missions/{mission_id}/directives', {
      path: { mission_id: missionId },
      data: input,
    });
  },

  getMissionDirectives: async (missionId: string): Promise<UserDirective[]> => {
    return contractGet('/missions/{mission_id}/directives', { path: { mission_id: missionId } });
  },

  getMissionCanvas: async (missionId: string): Promise<MissionCanvas> => {
    const res = await contractGet('/missions/{mission_id}/canvas', {
      path: { mission_id: missionId },
    });
    return asView<MissionCanvas>(res.canvas);
  },

  getMissionTimeline: async (missionId: string): Promise<ApiAuditEvent[]> => {
    return asView<ApiAuditEvent[]>(await contractGet('/missions/{mission_id}/timeline', { path: { mission_id: missionId } }));
  },

  getProject: async (id: string): Promise<ApiProject> => {
    return asView<ApiProject>(await contractGet('/projects/{project_id}', { path: { project_id: id } }));
  },

  getWorkerCommandPolicy: async (): Promise<WorkerCommandPolicy> => {
    return asView<WorkerCommandPolicy>(await contractGet('/worker-command-policy'));
  },

  getProjectFindings: async (projectId: string): Promise<ApiFinding[]> => {
    return asView<ApiFinding[]>(await contractGet('/projects/{project_id}/findings', { path: { project_id: projectId } }));
  },

  getFindingAssetTree: async (projectId: string): Promise<FindingAssetTree> => {
    return asView<FindingAssetTree>(await contractGet('/projects/{project_id}/findings/tree', { path: { project_id: projectId } }));
  },

  getProjectAuditRuns: async (projectId: string): Promise<ApiAuditRun[]> => {
    return asView<ApiAuditRun[]>(await contractGet('/projects/{project_id}/audit/runs', { path: { project_id: projectId } }));
  },

  getProjectToolInvocations: async (projectId: string): Promise<ApiToolInvocation[]> => {
    return asView<ApiToolInvocation[]>(await contractGet('/projects/{project_id}/tool-invocations', {
      path: { project_id: projectId },
    }));
  },

  getToolInvocations: async (): Promise<ApiToolInvocation[]> => {
    return asView<ApiToolInvocation[]>(await contractGet('/tool-invocations'));
  },

  // ------------------------------------------------------------------
  // External Worker Runtimes
  // ------------------------------------------------------------------

  listWorkerRuns: async (query?: {
    project_id?: string;
    limit?: number;
  }): Promise<WorkerRun[]> => {
    return asView<WorkerRun[]>(await contractGet('/worker-runs', { query }));
  },

  // 项目级 GraphSnapshot：facts/intents/.../model_invocations/tasks 全量。
  // 注意：**不是** /missions/{id}/exploration-graph——那个端点只投出
  // {nodes, edges}，没有 model_invocations。要读思考过程必须用这个。
  getProjectGraph: async (projectId: string): Promise<GraphSnapshot> => {
    return asView<GraphSnapshot>(
      await contractGet('/projects/{project_id}/graph', { path: { project_id: projectId } }),
    );
  },

  listWorkerRuntimes: async (): Promise<WorkerProbe[]> => {
    return asView<WorkerProbe[]>(await contractGet('/worker-runtimes'));
  },

  getWorkerRuntime: async (runtimeId: string): Promise<WorkerProbe> => {
    return asView<WorkerProbe>(
      await contractGet('/worker-runtimes/{runtime_id}', { path: { runtime_id: runtimeId } }),
    );
  },

  refreshWorkerRuntimes: async (): Promise<WorkerProbe[]> => {
    return asView<WorkerProbe[]>(await contractPost('/worker-runtimes/refresh'));
  },
  missionAdvise: async (
    missionId: string,
    question: string,
    history: Array<{ role: 'user' | 'assistant'; content: string }> = [],
  ): Promise<{ answer: string; model: string | null }> => {
    return asView<{ answer: string; model: string | null }>(
      await contractPost('/missions/{mission_id}/advise', {
        path: { mission_id: missionId },
        data: { question, history },
        config: { timeout: ADVISOR_OPERATION_TIMEOUT_MS },
      }),
    );
  },
  interruptMission: async (
    missionId: string,
    message: string,
  ): Promise<{
    status: string;
    worker_run_id?: string | null;
    runtime?: string | null;
    session_known?: boolean | null;
  }> => {
    return asView<{
      status: string;
      worker_run_id?: string | null;
      runtime?: string | null;
      session_known?: boolean | null;
    }>(
      await contractPost('/missions/{mission_id}/interrupt', {
        path: { mission_id: missionId },
        data: { message },
      }),
    );
  },

  asyncIntake: async (input: {
    prompt: string;
    artifact_record_ids?: string[];
  }): Promise<{ mission: Mission }> => {
    return asView<{ mission: Mission }>(
      await contractPost('/intake/async', { data: input }),
    );
  },

  getGatewayStatus: async (): Promise<GatewayStatus> => {
    return asView<GatewayStatus>(await contractGet('/gateway/status'));
  },
  startGateway: async (): Promise<GatewayStatus> => {
    return asView<GatewayStatus>(await contractPost('/gateway/start'));
  },
  stopGateway: async (): Promise<GatewayStatus> => {
    return asView<GatewayStatus>(await contractPost('/gateway/stop'));
  },
  getGatewayUsage: async (
    days?: number,
    groupBy?: WorkerUsageDimension,
  ): Promise<WorkerUsageBreakdown> => {
    return asView<WorkerUsageBreakdown>(
      await contractGet('/gateway/usage', {
        query: {
          ...(days ? { days } : {}),
          ...(groupBy ? { group_by: groupBy } : {}),
        } as ContractQuery<'/gateway/usage', 'get'>,
      }),
    );
  },

  /** 仪表盘首行五卡一次性聚合（GET /stats/dashboard）。 */
  getDashboardStats: async (): Promise<DashboardStats> => {
    return asView<DashboardStats>(await contractGet('/stats/dashboard'));
  },

  // ── Agent presets（模型分工提示词）──────────────────────────────────

  listAgentPresets: async (): Promise<AgentPreset[]> => {
    return asView<AgentPreset[]>(await contractGet('/agent-presets'));
  },

  getAgentPreset: async (key: string): Promise<AgentPreset> => {
    return asView<AgentPreset>(
      await contractGet('/agent-presets/{key}', { path: { key } }),
    );
  },

  createAgentPreset: async (input: CreateAgentPresetRequest): Promise<AgentPreset> => {
    return asView<AgentPreset>(
      await contractPost('/agent-presets', { data: input }),
    );
  },

  updateAgentPreset: async (
    key: string,
    input: UpdateAgentPresetRequest,
  ): Promise<AgentPreset> => {
    return asView<AgentPreset>(
      await contractPatch('/agent-presets/{key}', { path: { key }, data: input }),
    );
  },

  deleteAgentPreset: async (key: string): Promise<void> => {
    await contractDelete('/agent-presets/{key}', { path: { key } });
  },

  // ── 探索链路（GET /missions/{id}/exploration-graph）────────────────

  getExplorationGraph: async (missionId: string): Promise<ExplorationGraph> => {
    return asView<ExplorationGraph>(
      await contractGet('/missions/{mission_id}/exploration-graph', {
        path: { mission_id: missionId },
      }),
    );
  },

  // ── Skills（SKILL.md 目录）──────────────────────────────────────────

  listSkills: async (): Promise<SkillMeta[]> => {
    return asView<SkillMeta[]>(await contractGet('/skills'));
  },

  getSkill: async (name: string): Promise<{ meta: SkillMeta; manual: string }> => {
    return asView<{ meta: SkillMeta; manual: string }>(
      await contractGet('/skills/{name}', { path: { name } }),
    );
  },

  createSkill: async (input: CreateSkillRequest): Promise<{ created: string }> => {
    return asView<{ created: string }>(await contractPost('/skills', { data: input }));
  },

  uploadSkillZip: async (file: File): Promise<{ imported: string }> => {
    const formData = new FormData();
    formData.append('file', file);
    return asView<{ imported: string }>(
      await contractPost('/skills/upload', { data: formData }),
    );
  },

  deleteSkill: async (name: string): Promise<void> => {
    await contractDelete('/skills/{name}', { path: { name } });
  },

  getSkillMissing: async (): Promise<SkillMissingEntry[]> => {
    return asView<SkillMissingEntry[]>(await contractGet('/skills/missing'));
  },

  getSkillUsage: async (name: string): Promise<SkillUsageRow[]> => {
    return asView<SkillUsageRow[]>(
      await contractGet('/skills/{name}/usage', { path: { name } }),
    );
  },

  listSkillFiles: async (name: string): Promise<{ files: string[] }> => {
    return asView<{ files: string[] }>(
      await contractGet('/skills/{name}/files', { path: { name } }),
    );
  },

  readSkillFile: async (name: string, path: string): Promise<{ path: string; content: string }> => {
    return asView<{ path: string; content: string }>(
      await contractGet('/skills/{name}/file', { path: { name }, query: { path } }),
    );
  },

  writeSkillFile: async (
    name: string,
    path: string,
    content: string,
  ): Promise<{ written: string }> => {
    return asView<{ written: string }>(
      await contractPut('/skills/{name}/file', {
        path: { name },
        query: { path },
        data: { content },
      }),
    );
  },

  previewAgentPreset: async (
    key: string,
    variables: Record<string, string>,
  ): Promise<AgentPresetPreviewResponse> => {
    return asView<AgentPresetPreviewResponse>(
      await contractPost('/agent-presets/{key}/preview', {
        path: { key },
        data: { variables },
      }),
    );
  },

  listWorkerRuntimeProfiles: async (): Promise<WorkerRuntimeProfile[]> => {
    return asView<WorkerRuntimeProfile[]>(await contractGet('/worker-runtime-profiles'));
  },

  upsertWorkerRuntimeProfile: async (
    input: UpsertWorkerRuntimeProfileRequest,
  ): Promise<WorkerRuntimeProfile> => {
    return asView<WorkerRuntimeProfile>(
      await contractPost('/worker-runtime-profiles', { data: input }),
    );
  },

  deleteWorkerRuntimeProfile: async (profileId: string): Promise<void> => {
    await contractDelete('/worker-runtime-profiles/{profile_id}', {
      path: { profile_id: profileId },
    });
  },

  getToolCatalog: async (): Promise<ToolCatalogEntry[]> => {
    return contractGet('/tool-catalog');
  },

  getToolCatalogStatus: async (): Promise<ToolDetectionStatus> => {
    return contractGet('/tool-catalog/status');
  },

  refreshToolCatalog: async (): Promise<ToolDetectionStatus> => {
    return contractPost('/tool-catalog/refresh');
  },

  configureToolPath: async (
    toolId: string,
    input: {
      executable_path: string | null;
      enabled: boolean;
      /** 调用参数整体替换集（按 invocation 声明校验）；省略 = 保持已存值。 */
      params?: Record<string, unknown>;
      /** env 逐 key 合并集；null/空串清除该 key。明文保存后不回显。 */
      env?: Record<string, string | null>;
    },
  ): Promise<ToolCatalogEntry> => {
    return contractPost('/tool-catalog/{tool_id}/configure', {
      path: { tool_id: toolId },
      data: input,
    });
  },

  testToolCatalog: async (toolId: string): Promise<ModuleHealthResult> => {
    return asView<ModuleHealthResult>(await contractPost('/tool-catalog/{tool_id}/test', { path: { tool_id: toolId } }));
  },

  installToolCatalog: async (
    toolId: string,
    input: { force?: boolean } = {},
  ): Promise<ToolInstallJob> => {
    return contractPost('/tool-catalog/{tool_id}/install', {
      path: { tool_id: toolId },
      data: input,
    });
  },

  getToolInstallations: async (toolId?: string): Promise<ToolInstallJob[]> => {
    return contractGet('/tool-catalog/installations', {
      query: toolId ? { tool_id: toolId } : undefined,
    });
  },

  getToolInstallation: async (jobId: string): Promise<ToolInstallJob> => {
    return contractGet('/tool-catalog/installations/{job_id}', { path: { job_id: jobId } });
  },

  getToolRecommendations: async (params?: { project_id?: string; mission_id?: string }): Promise<ToolRecommendation[]> => {
    return contractGet('/tool-catalog/recommendations', { query: params });
  },

  createProject: async (input: {
    name: string;
    audit_domain: AuditDomain | string;
    description?: string;
    target?: Record<string, unknown>;
    goal?: string;
  }): Promise<ApiProject> => {
    return asView<ApiProject>(await contractPost('/projects', {
      data: input as ContractRequestBody<'/projects', 'post'>,
    }));
  },

  deleteProject: async (projectId: string): Promise<void> => {
    await contractDelete('/projects/{project_id}', { path: { project_id: projectId } });
  },

  getProviders: async (): Promise<ProviderConfigResponse[]> => {
    return contractGet('/providers');
  },

  getProvider: async (id: string): Promise<ProviderConfigResponse> => {
    return contractGet('/providers/{provider_id}', { path: { provider_id: id } });
  },

  getDefaultProvider: async (): Promise<ProviderConfigResponse | null> => {
    try {
      return await contractGet('/providers/default');
    } catch (e: unknown) {
      if ((e as { response?: { status?: number } }).response?.status === 404) return null;
      throw e;
    }
  },

  createProvider: async (input: CreateProviderRequest): Promise<ProviderConfigResponse> => {
    return contractPost('/providers', { data: input });
  },

  updateProvider: async (id: string, input: UpdateProviderRequest): Promise<ProviderConfigResponse> => {
    return contractPatch('/providers/{provider_id}', { path: { provider_id: id }, data: input });
  },

  deleteProvider: async (id: string): Promise<void> => {
    await contractDelete('/providers/{provider_id}', { path: { provider_id: id } });
  },

  testProvider: async (id: string): Promise<ProviderHealthResult> => {
    return contractPost('/providers/{provider_id}/test', {
      path: { provider_id: id },
      config: { timeout: MODEL_OPERATION_TIMEOUT_MS },
    });
  },

  discoverProviderModels: async (
    input: DiscoverProviderModelsRequest
  ): Promise<ProviderModelDiscoveryResult> => {
    return contractPost('/providers/discover-models', { data: input });
  },

  getProviderRoutes: async (purpose?: string): Promise<ProviderRouteBinding[]> => {
    return contractGet('/providers/routes', {
      query: purpose ? { purpose } : undefined,
    });
  },

  // ── Intelligence Hub ────────────────────────────────────────────────

  getIntelSources: async (): Promise<IntelSourceInfo[]> => {
    const response = await contractGet('/intelligence/sources');
    return asView<IntelSourceInfo[]>(response.sources);
  },

  runIntelQuery: async (input: IntelQuery): Promise<IntelExpansionReport> => {
    return asView<IntelExpansionReport>(
      await contractPost('/intelligence/query', {
        data: input as ContractRequestBody<'/intelligence/query', 'post'>,
      }),
    );
  },

  getIntelEntities: async (
    params: { kind?: IntelEntityKind; q?: string; limit?: number } = {},
  ): Promise<IntelEntityRecord[]> => {
    return asView<IntelEntityRecord[]>(
      await contractGet('/intelligence/entities', {
        query: params as ContractQuery<'/intelligence/entities', 'get'>,
      }),
    );
  },

  getIntelRelations: async (params: { entity_id?: string } = {}): Promise<IntelRelationRecord[]> => {
    return asView<IntelRelationRecord[]>(
      await contractGet('/intelligence/relations', {
        query: params as ContractQuery<'/intelligence/relations', 'get'>,
      }),
    );
  },

  getIntelRawRecords: async (params: { source?: string; limit?: number } = {}): Promise<IntelRawRecord[]> => {
    return asView<IntelRawRecord[]>(
      await contractGet('/intelligence/raw-records', {
        query: params as ContractQuery<'/intelligence/raw-records', 'get'>,
      }),
    );
  },

  promoteIntelEntity: async (entityId: string, missionId: string): Promise<IntelPromoteOutcome> => {
    return asView<IntelPromoteOutcome>(
      await contractPost('/intelligence/entities/{entity_id}/promote', {
        path: { entity_id: entityId },
        data: { mission_id: missionId },
      }),
    );
  },

  createProviderRoute: async (input: Partial<ProviderRouteBinding> & { purpose: string; provider_id: string }): Promise<ProviderRouteBinding> => {
    return contractPost('/providers/routes', { data: input });
  },

  updateProviderRoute: async (routeId: string, input: Partial<ProviderRouteBinding> & { purpose: string; provider_id: string }): Promise<ProviderRouteBinding> => {
    return contractPatch('/providers/routes/{route_id}', { path: { route_id: routeId }, data: input });
  },

  deleteProviderRoute: async (id: string): Promise<void> => {
    await contractDelete('/providers/routes/{route_id}', { path: { route_id: id } });
  },

  getModules: async (): Promise<ApiModule[]> => {
    return asView<ApiModule[]>(await contractGet('/modules'));
  },

  getModuleHealth: async (): Promise<Record<string, ModuleHealthResult>> => {
    return asView<Record<string, ModuleHealthResult>>(await contractGet('/modules/health'));
  },

  getModule: async (id: string): Promise<ApiModule> => {
    return asView<ApiModule>(await contractGet('/modules/{module_id}', { path: { module_id: id } }));
  },

  createModule: async (input: CreateModuleRequest): Promise<ApiModule> => {
    return asView<ApiModule>(await contractPost('/modules', { data: input }));
  },

  updateModule: async (id: string, input: UpdateModuleRequest): Promise<ApiModule> => {
    return asView<ApiModule>(await contractPatch('/modules/{module_id}', { path: { module_id: id }, data: input }));
  },

  deleteModule: async (id: string): Promise<void> => {
    await contractDelete('/modules/{module_id}', { path: { module_id: id } });
  },

  // Decision Gates
  getDecisionGates: async (params?: Record<string, unknown>): Promise<DecisionGate[]> => {
    return asView<DecisionGate[]>(await contractGet('/decision-gates', {
      query: params as ContractQuery<'/decision-gates', 'get'>,
    }));
  },

  getProjectDecisionGates: async (projectId: string, params?: Record<string, unknown>): Promise<DecisionGate[]> => {
    return asView<DecisionGate[]>(await contractGet('/projects/{project_id}/decision-gates', {
      path: { project_id: projectId },
      query: params as ContractQuery<'/projects/{project_id}/decision-gates', 'get'>,
    }));
  },

  getRunDecisionGates: async (projectId: string, runId: string, params?: Record<string, unknown>): Promise<DecisionGate[]> => {
    return asView<DecisionGate[]>(await contractGet('/projects/{project_id}/audit/runs/{run_id}/decision-gates', {
      path: { project_id: projectId, run_id: runId },
      query: params as ContractQuery<'/projects/{project_id}/audit/runs/{run_id}/decision-gates', 'get'>,
    }));
  },

  getDecisionGate: async (id: string): Promise<DecisionGate> => {
    return asView<DecisionGate>(await contractGet('/decision-gates/{gate_id}', { path: { gate_id: id } }));
  },

  answerDecisionGate: async (id: string, answer: DecisionAnswer): Promise<DecisionGate> => {
    return asView<DecisionGate>(await contractPost('/decision-gates/{gate_id}/answer', { path: { gate_id: id }, data: answer }));
  },

  cancelDecisionGate: async (id: string): Promise<DecisionGate> => {
    return asView<DecisionGate>(await contractPost('/decision-gates/{gate_id}/cancel', { path: { gate_id: id } }));
  },

  resumeAuditRun: async (projectId: string, runId: string): Promise<ApiAuditRun> => {
    return asView<ApiAuditRun>(await contractPost('/projects/{project_id}/audit/runs/{run_id}/resume', {
      path: { project_id: projectId, run_id: runId },
    }));
  },

  getProjectObservations: async (projectId: string, runId?: string): Promise<Observation[]> => {
    return asView<Observation[]>(await contractGet('/projects/{project_id}/observations', {
      path: { project_id: projectId },
      query: runId ? { run_id: runId } : undefined,
    }));
  },

  getProjectReflectorReports: async (projectId: string, runId?: string): Promise<ReflectorReport[]> => {
    return asView<ReflectorReport[]>(await contractGet('/projects/{project_id}/reflector-reports', {
      path: { project_id: projectId },
      query: runId ? { run_id: runId } : undefined,
    }));
  },

  getProjectTerminationAssessments: async (projectId: string, runId?: string): Promise<TerminationAssessment[]> => {
    return contractGet('/projects/{project_id}/termination-assessments', {
      path: { project_id: projectId },
      query: runId ? { run_id: runId } : undefined,
    });
  },

  getProjectWorkerLeases: async (projectId: string, runId?: string): Promise<WorkerLease[]> => {
    return asView<WorkerLease[]>(await contractGet('/projects/{project_id}/worker-leases', {
      path: { project_id: projectId },
      query: runId ? { run_id: runId } : undefined,
    }));
  },

  // Strategy Board

  getStrategyBoardLatest: async (projectId: string, runId?: string): Promise<StrategyBoardSnapshot> => {
    return asView<StrategyBoardSnapshot>(await contractGet('/projects/{project_id}/strategy-board/latest', {
      path: { project_id: projectId },
      query: runId ? { run_id: runId } : undefined,
    }));
  },

  getStrategyBoardSnapshots: async (projectId: string, runId?: string): Promise<StrategyBoardSnapshot[]> => {
    return asView<StrategyBoardSnapshot[]>(await contractGet('/projects/{project_id}/strategy-board/snapshots', {
      path: { project_id: projectId },
      query: runId ? { run_id: runId } : undefined,
    }));
  },

  // ==========================================
  // Knowledge Cards
  // ==========================================
  addKnowledgeCard: async (input: Omit<KnowledgeCard, 'id'|'created_at'|'updated_at'>): Promise<KnowledgeCard> => {
    return contractPost('/knowledge/cards', { data: input });
  },

  listKnowledgeCards: async (): Promise<KnowledgeCard[]> => {
    return contractGet('/knowledge/cards');
  },

  searchKnowledgeCards: async (input: KnowledgeRetrievalQuery): Promise<KnowledgeRetrievalResult[]> => {
    return contractPost('/knowledge/cards/search', { data: { query: input } });
  },

  /** 知识语料/FTS 索引状态（empty/ready/stale）。 */
  knowledgeIndexStatus: async (): Promise<KnowledgeCorpusStatus> => {
    return contractGet('/knowledge/index-status');
  },

  /** 全量重建知识 FTS 索引（批量导入后调用）。 */
  knowledgeIndexSync: async (): Promise<KnowledgeCorpusStatus> => {
    return contractPost('/knowledge/index-sync', { data: {} });
  },

  listArtifacts: async (projectId?: string, runId?: string): Promise<ArtifactRecord[]> => {
    const query: Record<string, string> = {};
    if (projectId) query.project_id = projectId;
    if (runId) query.run_id = runId;
    return contractGet('/artifacts', { query });
  },

  listRuntimeSettings: async (projectId?: string, runId?: string): Promise<RuntimeSetting[]> => {
    const query: Record<string, string> = {};
    if (projectId) query.project_id = projectId;
    if (runId) query.run_id = runId;
    return contractGet('/runtime-settings', { query });
  },

  upsertRuntimeSetting: async (
    key: string,
    input: { value: Record<string, unknown>; scope?: 'global' | 'project' | 'run'; project_id?: string | null; run_id?: string | null; description?: string; updated_by?: string }
  ): Promise<RuntimeSetting> => {
    return contractPut('/runtime-settings/{key}', { path: { key }, data: input });
  },

  // ==========================================
  // Uploads
  // ==========================================
  uploadArtifact: async (input: {
    file: File;
    purpose?: string;
    project_id?: string;
    mission_id?: string;
    run_id?: string;
  }): Promise<UploadArtifactResponse> => {
    const formData = new FormData();
    formData.append('file', input.file);
    if (input.purpose) formData.append('purpose', input.purpose);
    if (input.project_id) formData.append('project_id', input.project_id);
    if (input.mission_id) formData.append('mission_id', input.mission_id);
    if (input.run_id) formData.append('run_id', input.run_id);
    return contractPost('/uploads', { data: formData });
  },

  getMissionAssets: async (missionId: string): Promise<MissionAsset[]> => {
    return contractGet('/missions/{mission_id}/assets', { path: { mission_id: missionId } });
  },

  // ==========================================
  // Intake
  // ==========================================
  analyzeIntake: async (input: IntakeAnalyzeRequest): Promise<IntakeAnalyzeResponse> => {
    return asView<IntakeAnalyzeResponse>(await contractPost('/intake/analyze', {
      data: input,
      config: { timeout: MODEL_OPERATION_TIMEOUT_MS },
    }));
  },

  createProjectFromIntake: async (input: IntakeStartRequest): Promise<IntakeStartResponse> => {
    return asView<IntakeStartResponse>(await contractPost('/intake/create-project', {
      data: input as ContractRequestBody<'/intake/create-project', 'post'>,
    }));
  },

  startIntake: async (input: IntakeStartRequest): Promise<IntakeStartResponse> => {
    return asView<IntakeStartResponse>(await contractPost('/intake/start', {
      data: input as ContractRequestBody<'/intake/start', 'post'>,
    }));
  },

  getAgentNarratives: async (
    projectId: string,
    params?: { run_id?: string; mission_id?: string; branch_id?: string; limit?: number }
  ): Promise<AgentNarrativeEvent[]> => {
    return asView<AgentNarrativeEvent[]>(await contractGet('/projects/{project_id}/agent-narratives', {
      path: { project_id: projectId },
      query: params,
    }));
  },

  createAgentNarrative: async (
    projectId: string,
    payload: CreateAgentNarrativeRequest
  ): Promise<AgentNarrativeEvent> => {
    return asView<AgentNarrativeEvent>(await contractPost('/projects/{project_id}/agent-narratives', {
      path: { project_id: projectId },
      data: payload,
    }));
  },

  // ==========================================
  // README API Aliases (mission-first surface)
  // ==========================================

  /** README alias: GET /engines (Engine Pool view) */
  getEngines: async (): Promise<EnginePoolEntry[]> => {
    return contractGet('/engines');
  },

  /** README alias: GET /missions/{id}/evidence */
  getMissionEvidence: async (missionId: string): Promise<MissionEvidenceSummary> => {
    return contractGet('/missions/{mission_id}/evidence', { path: { mission_id: missionId } });
  },

  /** README alias: POST /missions/{id}/signal */
  postMissionSignal: async (missionId: string, payload: MissionSignalRequest): Promise<MissionSignalResponse> => {
    return contractPost('/missions/{mission_id}/signal', {
      path: { mission_id: missionId },
      data: payload,
    });
  },
};