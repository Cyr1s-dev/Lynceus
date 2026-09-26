export type Severity = 'info' | 'low' | 'medium' | 'high' | 'critical';
export type FindingStatus = 'candidate' | 'needs_review' | 'confirmed' | 'false_positive' | 'duplicate' | 'gap' | 'phenomenon' | 'fixed';
export type RunStatus = 'pending' | 'running' | 'paused' | 'waiting_for_decision' | 'reviewing' | 'reporting' | 'completed' | 'failed' | 'cancelled';
export type ToolStatus = 'ok' | 'waiting_for_confirmation' | 'error' | 'timeout' | 'denied';
export type AuditDomain =
  | 'asset_recon'
  | 'web_recon'
  | 'web_sast'
  | 'web_dast'
  | 'web_iast'
  | 'web_validation'
  | 'traffic_intelligence'
  | 'code_deep_sast'
  | 'binary_static'
  | 'binary_dynamic'
  | 'exploitability'
  | 'fuzzing'
  | 'supply_chain'
  | 'cloud_native'
  | 'composite'
  | 'content_discovery'
  | 'fingerprint_intelligence'
  | 'exposure_intelligence'
  | 'exploitability_validation'
  | 'internal_surface'
  | 'misc';

export type ApprovalMode = 'ask_for_approval' | 'approve_for_me' | 'full_access';
export type MissionStatus = 'draft' | 'running' | 'paused' | 'waiting_for_decision' | 'completed' | 'failed' | 'cancelled';
export type BranchStatus = 'proposed' | 'active' | 'blocked' | 'succeeded' | 'failed' | 'abandoned' | 'superseded';
export type UserDirectiveType =
  | 'add_requirement'
  | 'pause'
  | 'resume'
  | 'prioritize_branch'
  | 'abandon_branch'
  | 'reopen_branch'
  | 'rollback_patch'
  | 'narrow_scope'
  | 'exclude_scope'
  | 'ask_question';
export type UserDirectiveStatus = 'pending' | 'applied' | 'rejected' | 'superseded';

/** 工具探测快照元信息（GET /tool-catalog/status、POST /tool-catalog/refresh）。 */
export interface ToolDetectionStatus {
  /** 当前后台探测生命周期。 */
  state: 'unknown' | 'refreshing' | 'ready' | 'error';
  /** 上次全量探测时间（RFC 3339）；从未探测为 null/缺省。 */
  detected_at?: string | null;
  /** 快照中的工具条数。 */
  detection_count: number;
  /** 其中可用的条数。 */
  available_count: number;
  /** 是否从未有过有效快照。 */
  stale: boolean;
  /** 最近一次 refresh 的有界错误。 */
  last_error?: string | null;
}

export interface ToolCatalogEntry {
  id: string;
  /** 运行时启停（local-tools.json enabled；未配置默认 true）。 */
  enabled: boolean;
  name: string;
  domain: string;
  description: string;
  upstream_url: string;
  executable_names: string[];
  version_args: string[];
  supported_platforms: string[];
  output_format: string;
  adapter_status: string;
  risk_notes: string[];
  install?: {
    method: 'go' | 'pip' | 'cargo' | 'github_release' | 'manual';
    package?: string | null;
    version?: string | null;
    repository?: string | null;
    release_tag?: string | null;
    asset_patterns?: Record<string, string>;
    sha256_by_platform?: Record<string, string>;
    archive_executable_patterns?: Record<string, string>;
    include_archive_files?: string[];
    rename_executable?: boolean;
    enabled_by_default?: boolean;
    requires_cgo?: boolean;
    note?: string | null;
  } | null;
  detection: {
    available: boolean;
    availability: 'unknown' | 'configured' | 'path' | 'missing';
    executable_path?: string | null;
    source?: string | null;
  };
  /** 声明式调用面（参数 + env key）；存在即代表该工具可配置。 */
  invocation?: {
    params?: Array<{
      key: string;
      kind: 'string' | 'integer' | 'boolean' | 'string_list' | 'path';
      flag?: string | null;
      default?: string | number | boolean | string[] | null;
      required?: boolean;
      description?: string | null;
      minimum?: number | null;
      maximum?: number | null;
    }> | null;
    env_keys?: Array<{
      name: string;
      description?: string | null;
      required?: boolean;
    }> | null;
  } | null;
  /** 脱敏回显：params 原样，env 仅已配置 key 名单（值永不回显）。 */
  configured_settings?: {
    params?: Record<string, unknown>;
    env_set?: string[];
  } | null;
}

export type ToolInstallJobStatus =
  | 'queued'
  | 'running'
  | 'installed'
  | 'already_present'
  | 'manual'
  | 'skipped'
  | 'unsupported'
  | 'failed';

export interface ToolInstallJob {
  id: string;
  tool_id: string;
  method: string;
  force: boolean;
  status: ToolInstallJobStatus;
  message: string;
  executable_path?: string | null;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
}

export interface ToolRecommendation {
  tool_id: string;
  name: string;
  domain: string;
  priority: number;
  rationale: string;
  installed: boolean;
  adapter_status: string;
}

export interface BreakthroughCandidate {
  id: string;
  project_id: string;
  run_id?: string;
  target?: string;
  title: string;
  summary: string;
  signal_type?: string;
  priority_score: number;
  confidence: number;
  exploitability_clues?: string[];
  recommended_next_actions?: string[];
  related_fact_ids?: string[];
  related_evidence_ids?: string[];
  related_finding_ids?: string[];
  related_tool_invocation_ids?: string[];
  signal_ids?: string[];
  metadata?: Record<string, unknown>;
  created_at: string;
}

export interface ExposureSignal {
  id: string;
  project_id: string;
  run_id?: string;
  target?: string;
  signal_type: string;
  title: string;
  summary: string;
  score_delta: number;
  confidence: number;
  related_fact_ids?: string[];
  related_evidence_ids?: string[];
  related_finding_ids?: string[];
  related_tool_invocation_ids?: string[];
  metadata?: Record<string, unknown>;
  created_at: string;
}

export interface ExploitabilityAssessment {
  id: string;
  project_id: string;
  run_id?: string;
  finding_id: string;
  verdict: string;
  proof_summary: string;
  impact_summary: string;
  confidence: number;
  related_evidence_ids?: string[];
  related_tool_invocation_ids?: string[];
  created_at: string;
}

export interface Mission {
  id: string;
  project_id: string;
  user_goal: string;
  title?: string | null;
  target: Record<string, string>;
  constraints: string[];
  success_criteria: string[];
  goal_contract: MissionGoalContract;
  tags: string[];
  category?: string | null;
  approval_mode: ApprovalMode;
  archived: boolean;
  status: MissionStatus;
  active_run_id?: string | null;
  created_at: string;
  updated_at: string;
  finished_at?: string | null;
  created_by: string;
  metadata: Record<string, unknown>;
}

export interface SwarmOperationRecord {
  id: string;
  sequence: number;
  project_id: string;
  mission_id?: string | null;
  run_id: string;
  branch_id?: string | null;
  task_id?: string | null;
  worker_id?: string | null;
  provider_id?: string | null;
  model?: string | null;
  role: string;
  actor_label: string;
  operation_type: string;
  entry: string;
  payload: Record<string, unknown>;
  source_ids: string[];
  created_at: string;
}

export type GoalContractStatus = 'resolved' | 'needs_review';
export type GoalOutcomeType =
  | 'flag_capture'
  | 'confirmed_finding'
  | 'verified_evidence'
  | 'coverage'
  | 'custom';

export interface MissionGoalContract {
  schema_version: string;
  status: GoalContractStatus;
  outcome_type: GoalOutcomeType;
  description: string;
  minimum_count: number;
  finding_rule_ids: string[];
  evidence_kinds: string[];
  require_confirmed_findings: boolean;
  require_provenance: boolean;
  auto_complete: boolean;
  source: 'operator' | 'intake_model' | 'deterministic_fallback' | 'legacy';
  confidence: number;
  rationale?: string | null;
}

export interface SwarmOperationPage {
  records: SwarmOperationRecord[];
  after_sequence: number;
  next_after_sequence: number;
  total: number;
  has_more: boolean;
  latest_sequence: number;
}

export interface Branch {
  id: string;
  project_id: string;
  mission_id: string;
  run_id?: string | null;
  parent_branch_id?: string | null;
  title: string;
  hypothesis: string;
  rationale: string;
  status: BranchStatus;
  priority: number;
  confidence: number;
  budget_steps: number;
  steps_used: number;
  related_fact_ids: string[];
  related_evidence_ids: string[];
  related_finding_ids: string[];
  related_tool_invocation_ids: string[];
  assigned_worker_id?: string | null;
  created_by: string;
  created_at: string;
  updated_at: string;
  metadata: Record<string, unknown>;
}

export interface UserDirective {
  id: string;
  project_id: string;
  mission_id: string;
  run_id?: string | null;
  branch_id?: string | null;
  directive_type: UserDirectiveType;
  content: string;
  parsed_intent: Record<string, unknown>;
  status: UserDirectiveStatus;
  created_at: string;
  applied_at?: string | null;
  created_by: string;
  metadata: Record<string, unknown>;
}

export type DecisionGateStatus = 'pending' | 'answered' | 'cancelled' | 'expired';
export type DecisionGateKind = 'blocking' | 'advisory' | 'review';
export type DecisionSeverity = 'low' | 'medium' | 'high' | 'critical';

export interface DecisionOption {
  id: string;
  label: string;
  description?: string;
  impact?: string;
  risk?: string;
}

export interface DecisionAnswer {
  option_id: string;
  rationale?: string;
  freeform_text?: string;
}

export interface DecisionGate {
  id: string;
  project_id: string;
  audit_run_id: string;
  task_id?: string;
  question: string;
  context_summary?: string;
  kind: DecisionGateKind;
  severity: DecisionSeverity;
  status: DecisionGateStatus;
  options: DecisionOption[];
  recommended_option_id?: string;
  related_fact_ids?: string[];
  related_evidence_ids?: string[];
    related_finding_ids?: string[];
    metadata?: Record<string, unknown>;
  answer?: DecisionAnswer;
  answered_at?: string;
  answered_by?: string;
  created_at: string;
  updated_at: string;
  expires_at?: string;
}

export type ModuleType = 'builtin' | 'local_tool' | 'mcp_remote';
export type ModuleDomain = AuditDomain;
export type ModuleTransport = 'streamable_http' | 'sse' | 'stdio' | 'none';
export type ModuleProfile = 'full_access' | 'readonly' | 'standard' | 'unsafe';
export type ModuleRiskLevel = 'safe' | 'sensitive' | 'unsafe';

export type ModuleDiscoveryStatus =
  | 'pending'
  | 'probed'
  | 'proposed'
  | 'approved'
  | 'failed';

export interface DiscoveredMCPTool {
  name: string;
  description: string;
  input_schema: Record<string, unknown>;
  raw: Record<string, unknown>;
  discovered_at: string;
}

export interface ModuleDiscoverySession {
  id: string;
  user_prompt: string;
  pasted_text?: string | null;
  log_text?: string | null;
  endpoint_url?: string | null;
  transport?: ModuleTransport | null;
  module_type: ModuleType;
  domain_hint?: ModuleDomain | null;
  profile: ModuleProfile;
  provider_id?: string | null;
  model_override?: string | null;
  status: ModuleDiscoveryStatus;
  error?: string | null;
  discovered_tools: DiscoveredMCPTool[];
  created_at: string;
  updated_at: string;
}

export interface ModuleConfigProposal {
  id: string;
  discovery_session_id: string;
  proposed_name: string;
  module_type: ModuleType;
  domain: ModuleDomain;
  endpoint_url?: string | null;
  transport: ModuleTransport;
  profile: ModuleProfile;
  tool_allowlist: string[];
  capability_map: Record<string, string>;
  metadata: Record<string, unknown>;
  rationale: string;
  confidence: number;
  raw_tools: DiscoveredMCPTool[];
  provider_id?: string | null;
  model_override?: string | null;
  model_invocation_id?: string | null;
  approved_module_id?: string | null;
  created_at: string;
  updated_at: string;
}

export interface ApiModule {
  id: string;
  name: string;
  module_type: ModuleType;
  domain: ModuleDomain;
  endpoint_url?: string;
  transport: ModuleTransport;
  enabled: boolean;
  profile: ModuleProfile;
  tool_allowlist: string[];
  capability_map: Record<string, string>;
  metadata: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

export interface CreateModuleRequest {
  name: string;
  module_type: ModuleType;
  domain: ModuleDomain;
  endpoint_url?: string;
  transport: ModuleTransport;
  enabled?: boolean;
  profile: ModuleProfile;
  tool_allowlist?: string[];
  capability_map?: Record<string, string>;
  metadata?: Record<string, unknown>;
}

export interface UpdateModuleRequest {
  name?: string;
  module_type?: ModuleType;
  domain?: ModuleDomain;
  endpoint_url?: string;
  transport?: ModuleTransport;
  enabled?: boolean;
  profile?: ModuleProfile;
  tool_allowlist?: string[];
  capability_map?: Record<string, string>;
  metadata?: Record<string, unknown>;
}

export interface ModuleHealthResult {
  module_id: string;
  ok: boolean;
  status: string;
  message: string;
  checked_at: string;
  latency_ms?: number;
  raw?: Record<string, unknown>;
}

export interface ModuleCapability {
  name: string;
  raw_tool_name: string;
  domain: ModuleDomain;
  risk_level: ModuleRiskLevel;
  description: string;
  input_schema: Record<string, unknown>;
  enabled: boolean;
}

export type RetrievalSourceKind =
  | 'knowledge_card'
  | 'source_file'
  | 'sarif'
  | 'evidence'
  | 'finding'
  | 'tool_invocation'
  | 'model_invocation'
  | 'ida_decompile'
  | 'traffic_artifact'
  | 'artifact'
  | 'strategy_board'
  | 'context_pack';

export type RetrievalStatus = 'hit' | 'empty' | 'low_score' | 'timeout' | 'error' | 'unavailable';

export interface RetrievedEvidence {
  chunk_id: string;
  source_kind: RetrievalSourceKind;
  source_id: string;
  score: number;
  rank: number;
  snippet: string;
  title: string;
  artifact_record_id?: string | null;
  artifact_uri?: string | null;
  artifact_sha256?: string | null;
  evidence_id?: string | null;
  finding_id?: string | null;
  tool_invocation_id?: string | null;
  model_invocation_id?: string | null;
  location: Record<string, unknown>;
  metadata: Record<string, unknown>;
}

export interface RetrievalInvocation {
  id: string;
  project_id: string;
  run_id?: string | null;
  task_id?: string | null;
  purpose: string;
  query_text: string;
  query_hash?: string | null;
  status: RetrievalStatus;
  reason: string;
  top_k: number;
  fetch_multiplier: number;
  min_score: number;
  token_budget: number;
  candidate_count: number;
  filtered_count: number;
  max_score: number;
  retrieved: RetrievedEvidence[];
  duration_ms?: number | null;
  cached: boolean;
  error?: string | null;
  created_at: string;
}

export interface ArtifactRecord {
  id: string;
  project_id?: string | null;
  run_id?: string | null;
  task_id?: string | null;
  tool_invocation_id?: string | null;
  model_invocation_id?: string | null;
  evidence_id?: string | null;
  finding_id?: string | null;
  kind: string;
  uri: string;
  storage_backend: string;
  summary: string;
  mime_type?: string | null;
  size_bytes?: number | null;
  sha256?: string | null;
  metadata: Record<string, unknown>;
  created_at: string;
}

export interface UploadArtifactResponse {
  artifact: ArtifactRecord;
  detected_input_type: string;
  detected_target_type: string;
  sha256: string;
  size_bytes: number;
}

export interface MissionAsset {
  id: string;
  project_id: string;
  mission_id: string;
  asset_type: string;
  value: string;
  label?: string | null;
  sensitivity: string;
  confidence: number;
  source: string;
  source_id?: string | null;
  run_id?: string | null;
  evidence_ids: string[];
  finding_ids: string[];
  tool_invocation_ids: string[];
  tags: string[];
  metadata: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

export interface RuntimeSetting {
  id: string;
  key: string;
  value: Record<string, unknown>;
  scope: 'global' | 'project' | 'run';
  project_id?: string | null;
  run_id?: string | null;
  description: string;
  updated_by: string;
  created_at: string;
  updated_at: string;
}

export interface ProviderRouteBinding {
  id: string;
  purpose: string;
  provider_id: string;
  model_override?: string | null;
  priority: number;
  weight: number;
  enabled: boolean;
  fallback_group: string;
  required_capabilities: Record<string, boolean>;
  max_failures: number;
  cooldown_seconds: number;
  failure_count: number;
  circuit_open_until?: string | null;
  metadata: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

export interface ApiProject {
  id: string;
  name: string;
  audit_domain: AuditDomain;
  description?: string;
  repository_url?: string;
  target?: {
    repo_path?: string;
    local_path?: string;
    path?: string;
    repo?: string;
    [key: string]: unknown;
  };
  created_at: string;
  updated_at: string;
}

export interface ApiFinding {
  id: string;
  title: string;
  severity: Severity;
  status: FindingStatus;
  project_id: string;
  mission_id?: string | null;
  rule_id?: string;
  cwe?: string | string[];
  produced_by_task_id?: string;
  evidence_ids?: string[];
  related_fact_ids?: string[];
  source_label?: string;
  sink_label?: string;
  confidence?: number;
  description?: string;
  fingerprint?: string;
  updated_at: string;
}

/** 跨任务资产树节点（后端 `GET /projects/{id}/findings/tree`）。 */
export interface FindingAssetNode {
  key: string;
  kind:
    | 'mission'
    | 'root_domain'
    | 'subdomain'
    | 'ip'
    | 'app'
    | 'service'
    | 'endpoint'
    | 'other';
  label: string;
  value: string;
  parent?: string | null;
  critical: number;
  high: number;
  total: number;
}

/** 跨任务资产树（项目级，跨 mission 合并同类资产）。 */
export interface FindingAssetTree {
  nodes: FindingAssetNode[];
  finding_total: number;
}

export interface ApiAuditRun {
  id: string;
  project_id: string;
  mission_id?: string | null;
  status: RunStatus;
  config?: {
    audit_domains?: AuditDomain[] | string[];
    resolved_module_ids?: string[];
    [key: string]: unknown;
  };
  steps_used?: number;
  max_total_steps?: number;
  created_at: string;
  started_at?: string;
}

export interface WorkerCommandPolicy {
  denied_prefixes: string[];
  prompt_only_rules: string[];
}

export interface ApiToolInvocation {
  id: string;
  project_id: string;
  mission_id?: string | null;
  branch_id?: string | null;
  run_id?: string;
  task_id?: string;
  tool_name: string;
  module_id?: string | null;
  /** 发起本次调用的 Worker 身份（后端 ToolInvocation.worker_id）。
   *  与 module_id 分工不同：module_id 是"哪个模块提供的工具"，
   *  worker_id 是"哪个 worker 按下了按钮"。 */
  worker_id?: string | null;
  input_summary?: string;
  output_summary?: string;
  status: ToolStatus;
  exit_code?: number;
  duration_ms?: number;
  artifact_paths?: string[];
  error?: string;
  metadata?: Record<string, unknown>;
  started_at: string;
  finished_at?: string;
}

/* ── 资产覆盖图（GET /missions/{id}/coverage-graph） ── */

export type CoverageNodeKind =
  | 'mission'
  | 'root_domain'
  | 'subdomain'
  | 'ip'
  | 'app'
  | 'service'
  | 'endpoint'
  | 'other';

export interface CoverageNode {
  key: string;
  kind: CoverageNodeKind;
  label: string;
  value: string;
  asset_type: string | null;
  tested: boolean;
  confidence: number | null;
  finding_ids: string[];
  evidence_ids: string[];
  tool_invocation_ids: string[];
  metadata: Record<string, unknown>;
}

export interface CoverageEdge {
  from: string;
  to: string;
}

export interface CoverageStats {
  total: number;
  tested: number;
  by_kind: Record<string, number>;
}

export interface CoverageGraph {
  mission_id: string;
  nodes: CoverageNode[];
  edges: CoverageEdge[];
  stats: CoverageStats;
}

/* ── 报告（GET /reports/{report_id}） ── */

export type ReportScope = 'mission' | 'run' | 'project';

export interface ReportSeverityTally {
  info: number;
  low: number;
  medium: number;
  high: number;
  critical: number;
}

export interface ReportFinding {
  id: string;
  title: string;
  severity: string;
  status: string;
  cwe: string | null;
  rule_id: string | null;
  description: string | null;
  evidence_ids: string[];
  produced_by_task_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface ReportAsset {
  id: string;
  asset_type: string;
  value: string;
  confidence: number;
  tested: boolean;
}

export interface ReportToolTally {
  tool: string;
  total: number;
  errors: number;
}

export interface ReportPayload {
  id: string;
  scope: ReportScope;
  title: string;
  generated_at: string;
  created_at: string;
  finding_total: number;
  severity: ReportSeverityTally;
  status: Record<string, number>;
  asset_total: number;
  assets_by_type: Record<string, number>;
  assets_tested: number;
  tool_total: number;
  tools: ReportToolTally[];
  findings: ReportFinding[];
  assets: ReportAsset[];
}

export interface ReportEnvelope {
  id: string;
  status: 'ready' | 'not_ready';
  reason: string | null;
  available_formats: string[];
  payload: ReportPayload | null;
  /** `format=markdown` 时后端额外返回的正文。 */
  markdown?: string;
}

/* ── 漏洞复测（GET/POST /missions/{id}/findings/{fid}/retests） ── */

export type RetestVerdict = 'reproduced' | 'fixed' | 'inconclusive' | '';

export type RetestStatus = 'pending' | 'running' | 'completed' | 'failed' | 'stopped';

export interface FindingRetest {
  id: string;
  project_id: string;
  mission_id: string;
  finding_id: string;
  status: RetestStatus;
  verdict: RetestVerdict;
  notes: string;
  summary: string;
  evidence: string;
  error: string;
  model: string | null;
  /**
   * 复测上下文来源。`readonly_mcp_grant+inline_snapshot` = worker 既拿到后端
   * 内联的快照、也自己读了白板；`inline_snapshot_only` = 只依据内联快照（没
   * 拿到只读授权），结论置信度更低；`none` = 上下文为空，直接判失败。
   */
  context_source: 'readonly_mcp_grant+inline_snapshot' | 'inline_snapshot_only' | 'none' | null;
  assessment: string;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

export interface ApiAuditEvent {
  id: string;
  project_id: string;
  run_id?: string;
  task_id?: string;
  tool_invocation_id?: string;
  type: string;
  actor: string;
  title: string;
  message?: string;
  severity?: string;
  status?: string;
  data: Record<string, unknown>;
  created_at: string;
}

export interface GraphEdge {
  src: string;
  dst: string;
  relation: string;
}

export interface MissionCanvas {
  mission: Mission;
  branches: Branch[];
  observations: Observation[];
  runs: ApiAuditRun[];
  run_history?: ApiAuditRun[];
  tasks: Array<Record<string, unknown>>;
  tool_invocations: ApiToolInvocation[];
  findings: ApiFinding[];
  evidence: Array<Record<string, unknown>>;
  assets?: MissionAsset[];
  strategy_board_latest?: StrategyBoardSnapshot | null;
  directives: UserDirective[];
  decision_gates: DecisionGate[];
  termination_assessments?: TerminationAssessment[];
  edges: GraphEdge[];
}

export interface MissionStartResponse {
  mission: Mission;
  run: ApiAuditRun;
  branches: Branch[];
}

export interface GraphSnapshot {
  project_id: string;
  facts: unknown[];
  intents: Intent[];
  hints: unknown[];
  evidence: unknown[];
  findings: ApiFinding[];
  runs: ApiAuditRun[];
  /** 契约里是 `AgentTask[]`；这里只声明意图归属要用的字段。 */
  tasks: Array<{ id?: string; intent_id?: string | null; mission_id?: string | null }>;
  tool_invocations: ApiToolInvocation[];
  /** 模型调用（含 reasoning）。思考过程的唯一权威来源。 */
  model_invocations: ModelInvocation[];
  events: ApiAuditEvent[];
  decision_gates: DecisionGate[];
  strategy_board_snapshots: StrategyBoardSnapshot[];
  edges: GraphEdge[];
}

// View Models

export interface FindingView extends ApiFinding {
  project_name: string;
  engine: string;
  evidence_count: number;
  confidence: number;
}

export interface AuditRunView extends ApiAuditRun {
  project_name: string;
  current_solver: string;
  finding_count: number;
  steps_used: number;
  max_total_steps: number;
  started_at_display: string;
}

export interface AgentActivity {
  id: string;
  actor: string;
  action: string;
  timestamp: string;
  result: 'success' | 'error' | 'info';
  details?: string;
}

export interface EvidenceQuality {
  findings_without_evidence: number | null;
  evidence_without_fact: number | null;
  needs_review_count: number | null;
  confirmed_count: number | null;
  tool_error_count: number | null;
}

export interface ToolCoverage {
  id: string;
  name: string;
  status: 'configured' | 'available' | 'unavailable' | 'unknown' | 'missing' | 'path' | 'not_cataloged' | 'planned';
  message?: string;
  module_id?: string;
  module_enabled?: boolean;
  source?: string;
  adapter_status?: string;
}

export interface NextAction {
  id: string;
  title: string;
  type: string;
  priority: 'high' | 'medium' | 'low';
  target_id: string;
  count?: number;
}

export interface DashboardSummary {
  high_value_findings: FindingView[];
  active_runs: AuditRunView[];
  agent_activity: AgentActivity[];
  evidence_quality: EvidenceQuality;
  tool_coverage: ToolCoverage[];
  next_actions: NextAction[];
  projects?: ApiProject[];
  runs?: ApiAuditRun[];
  findings?: ApiFinding[];
  invocations?: ApiToolInvocation[];
}

export type ObservationType =
  | 'progress'
  | 'tool_result'
  | 'blockage'
  | 'evidence_gap'
  | 'failure_boundary'
  | 'contradiction'
  | 'tool_failure'
  | 'hypothesis'
  | 'hypothesis_update'
  | 'user_note'
  | 'decision'
  | 'terminal_state';

export interface Observation {
  id: string;
  project_id: string;
  mission_id?: string | null;
  branch_id?: string | null;
  run_id: string;
  task_id?: string;
  intent_id?: string;
  worker_id?: string;
  observation_type: ObservationType;
  source: string;
  summary: string;
  data?: Record<string, unknown>;
  related_fact_ids?: string[];
  related_evidence_ids?: string[];
  related_finding_ids?: string[];
  related_tool_invocation_ids?: string[];
  related_task_ids?: string[];
  related_intent_ids?: string[];
  created_at: string;
}

export type AdvisorRiskLevel = 'low' | 'medium' | 'high' | 'critical';

export interface AdvisorReview {
  id: string;
  project_id: string;
  run_id: string;
  trigger: string;
  summary: string;
  recommendations: string[];
  recommended_next_intents: string[];
  risk_level: AdvisorRiskLevel;
  related_observation_ids?: string[];
  risk_notes?: string;
  related_fact_ids?: string[];
  related_evidence_ids?: string[];
  related_finding_ids?: string[];
  created_at: string;
}

export type ReflectorFailureType =
  | 'tool_unavailable'
  | 'invalid_config'
  | 'timeout'
  | 'insufficient_context'
  | 'unsupported_target'
  | 'solver_error'
  | 'unknown';

export interface ReflectorReport {
  id: string;
  project_id: string;
  run_id: string;
  task_id?: string;
  failure_type: ReflectorFailureType;
  root_cause_summary: string;
  failure_modes: string[];
  lessons: string[];
  suggested_playbook_updates: string[];
  related_observation_ids?: string[];
  outcome: string;
  recommended_playbooks: string[];
  created_at: string;
}

export type TerminationStatus = 'continue' | 'pause' | 'complete' | 'needs_human_decision';

export interface TerminationAssessment {
  id: string;
  project_id: string;
  run_id: string;
  status: TerminationStatus;
  reasons: string[];
  coverage_summary: string;
  unresolved_intent_ids: string[];
  unresolved_branch_ids?: string[];
  evidence_gap_count: number;
  high_value_open_questions: string[];
  goal_satisfied?: boolean | null;
  goal_requirements?: string[];
  satisfied_goal_requirements?: string[];
  unmet_goal_requirements?: string[];
  confidence: number;
  created_at: string;
}

export type WorkerLeaseStatus = 'active' | 'released' | 'expired' | 'cancelled' | 'completed';

export interface WorkerLease {
  id: string;
  project_id: string;
  run_id: string;
  intent_id: string;
  worker_id: string;
  task_id?: string;
  status: WorkerLeaseStatus;
  lease_expires_at: string;
  heartbeat_at: string;
  created_at: string;
  updated_at: string;
  metadata?: Record<string, unknown>;
}

export interface ContextCompressionReport {
  source_counts: Record<string, number>;
  included_counts: Record<string, number>;
  dropped_counts: Record<string, number>;
  warnings: string[];
}

export interface ContextPackBuildResponse {
  context_pack: {
    summary: string;
    facts: unknown[];
    intents: unknown[];
    hints: unknown[];
    evidence_ids: string[];
    finding_ids: string[];
    tool_invocation_ids: string[];
    [key: string]: unknown;
  };
  report: ContextCompressionReport;
}

// ==========================================
// Strategy Board Types
// ==========================================

export type StrategyBoardDomain = 
  | 'general'
  | 'ctf_web'
  | 'ctf_pwn'
  | 'ctf_reverse'
  | 'ctf_crypto'
  | 'ctf_forensics'
  | 'ctf_misc'
  | 'ctf_blockchain'
  | 'vulnerability_research'
  | 'project_code_audit'
  | 'web_sast'
  | 'web_dast'
  | 'web_iast'
  | 'binary_static'
  | 'binary_dynamic'
  | 'exploitability'
  | 'malware_analysis'
  | 'incident_forensics'
  | 'cloud_native'
  | 'supply_chain'
  | 'remediation';

export type StrategyBoardIdeaStatus = 'pending' | 'testing' | 'verified' | 'failed' | 'skipped';
export type StrategyBoardMemoryKind = 'fact' | 'evidence' | 'failure_boundary' | 'constraint' | 'tool_behavior' | 'hint' | 'summary';
export type StrategyBoardOpType = 'idea_add' | 'idea_update' | 'idea_delete' | 'memory_add' | 'memory_update' | 'memory_delete' | 'board_merge' | 'efficiency_reminder';

export interface StrategyBoardIdea {
  id: string;
  status: StrategyBoardIdeaStatus;
  content: string;
  reason?: string;
  refs?: string[];
  updated_at: string;
  metadata?: Record<string, unknown>;
}

export interface StrategyBoardMemory {
  id: string;
  kind: StrategyBoardMemoryKind;
  content: string;
  reason?: string;
  refs?: string[];
  updated_at: string;
  metadata?: Record<string, unknown>;
}

export interface StrategyBoardOperation {
  type: StrategyBoardOpType;
  id?: string;
  status?: StrategyBoardIdeaStatus;
  kind?: StrategyBoardMemoryKind;
  content?: string;
  reason?: string;
  refs?: string[];
}

export interface StrategyBoardSnapshot {
  id: string;
  project_id: string;
  run_id: string;
  version: number;
  domain_profile: StrategyBoardDomain;
  ideas: StrategyBoardIdea[];
  memory: StrategyBoardMemory[];
  efficiency_reminders?: string[];
  source_snapshot_id?: string;
  provider_id?: string | null;
  model_invocation_id?: string | null;
  trigger?: string;
  created_at: string;
  created_by?: string;
}

export interface StrategyBoardPromptPayload {
  domain_profile: StrategyBoardDomain;
  trigger: string;
  token_budget?: number;
}

export interface StrategyBoardOpsPayload {
  domain_profile: StrategyBoardDomain;
  trigger: string;
  created_by: string;
  provider_id?: string | null;
  model_invocation_id?: string | null;
  ops: StrategyBoardOperation[];
}

export interface StrategyBoardRunPayload {
  provider_id?: string | null;
  domain_profile: StrategyBoardDomain;
  trigger: string;
  token_budget?: number;
}

export interface StrategyBoardPromptBuiltPayload {
  project_id: string;
  run_id: string;
  domain_profile: StrategyBoardDomain;
  snapshot_id: string;
  system_prompt: string;
  user_payload: Record<string, unknown>;
  messages: unknown[];
  knowledge_card_ids: string[];
}

export interface StrategyBoardPromptResponse {
  payload: StrategyBoardPromptBuiltPayload;
}

export interface StrategyBoardSnapshotResponse {
  snapshot: StrategyBoardSnapshot;
}

// ==========================================
// Knowledge Base Types
// ==========================================

export type KnowledgeCardKind = 
  | 'tool_usage'
  | 'vulnerability_pattern'
  | 'case_reference'
  | 'payload_strategy'
  | 'false_positive_pattern'
  | 'remediation_pattern'
  | 'cloud_attack_path'
  | 'binary_pattern';

export interface KnowledgeCard {
  id: string;
  title: string;
  kind: KnowledgeCardKind;
  content: string;
  /** 注入用摘要（后端写入路径恒等维护 content == summary）。 */
  summary?: string;
  /** 完整知识正文（命令、步骤、OPSEC、攻击链）。 */
  body?: string;
  tags?: string[];
  priority?: number;
  /** 结构化过滤字段（ingestion 期从正文提取）。 */
  aliases?: string[];
  tool?: string[];
  technique?: string[];
  platform?: string[];
  protocol?: string[];
  prerequisites?: string[];
  /** 来源（如 security-wiki:toolCommands:ysoserial）。 */
  source?: string | null;
  /** 来源内定位（源文件条目 id 等）。 */
  source_locator?: string | null;
  /** 父知识单元（Tool Summary Unit ← Command Unit）。 */
  parent_id?: string | null;
  metadata?: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

/** 知识语料/FTS 索引状态（Retrieval Substrate）。 */
export type KnowledgeCorpusState = 'empty' | 'ready' | 'stale';

export interface KnowledgeCorpusStatus {
  state: KnowledgeCorpusState;
  card_count: number;
  indexed_count: number;
  last_synced_at: string | null;
  reason: string;
}

export interface KnowledgeRetrievalQuery {
  text: string;
  kinds?: KnowledgeCardKind[];
  tags?: string[];
  limit?: number;
}

export interface KnowledgeRetrievalResult {
  card: KnowledgeCard;
  score: number;
  matched_terms?: string[];
}

// ==========================================
// Agent Pipeline Types
// ==========================================

export interface Intent {
  id: string;
  project_id: string;
  status: string;
  title: string;
  description?: string;
  source_fact_ids?: string[];
  solver?: string;
  priority: number;
  max_steps?: number;
  created_by?: string;
  run_id?: string | null;
  created_at?: string;
  updated_at?: string;
}

export interface PlanNextActionsRequest {
  run_id?: string | null;
  profile?: string;
  persist?: boolean;
}

export interface StartAgentPipelineRequest {
  run_id?: string | null;
  profile?: string;
}

// ==========================================
// Intake Types
// ==========================================

export interface IntakeAnalyzeRequest {
  prompt: string;
  provider_id?: string | null;
  artifact_record_ids?: string[];
}

export interface IntakeProjectDraft {
  name: string;
  audit_domain: AuditDomain;
  description?: string | null;
  target: Record<string, string>;
  goal: string;
}

export interface IntakePipelineDraft {
  profile: string;
  audit_domains: AuditDomain[];
  config: Record<string, unknown>;
  start_mode: 'pipeline' | 'audit';
}

export interface IntakePlan {
  project: IntakeProjectDraft;
  pipeline: IntakePipelineDraft;
  goal_contract: MissionGoalContract;
  recommended_intents: string[];
  artifact_record_ids?: string[];
  artifacts_summary?: Record<string, unknown>[];
  confidence: number;
  rationale?: string | null;
  metadata?: Record<string, unknown>;
}

export interface IntakeAnalyzeResponse {
  plan: IntakePlan;
  mission_draft?: Partial<Mission>;
  target?: Record<string, unknown>;
  provider_id?: string | null;
  model_invocation_id?: string | null;
  used_fallback: boolean;
  fallback_reason?: string | null;
}

export interface IntakeStartRequest {
  plan: IntakePlan;
  mission_id?: string | null;
  start_pipeline?: boolean;
  start_audit?: boolean;
  artifact_record_ids?: string[];
}

export interface IntakeStartResponse {
  project: ApiProject;
  mission?: Mission | null;
  plan: IntakePlan;
  intents: Intent[];
  run?: ApiAuditRun | null;
  created_intents: Intent[];
  created_run?: ApiAuditRun | null;
  pipeline_status: Record<string, unknown>;
}

// --- README API alias types ---

export interface EnginePoolEntry {
  id: string;
  name: string;
  domain: string;
  category: string;
  status: 'installed' | 'planned' | 'missing';
  health: 'ok' | 'error' | 'unknown';
  capabilities: string[];
  input_contract: string | null;
  output_contract: string | null;
  agent_usage_hint: string | null;
  install_suggestion: string | null;
}

export interface MissionEvidenceSummary {
  mission_id: string;
  evidence: Array<Record<string, unknown>>;
  findings: Array<Record<string, unknown>>;
  tool_invocations: Array<Record<string, unknown>>;
  citations: Array<Record<string, unknown>>;
  counts: Record<string, number>;
}

export type MissionSignalType =
  | 'confirm'
  | 'reject'
  | 'add_requirement'
  | 'pause'
  | 'resume'
  | 'abandon_branch'
  | 'prioritize_branch'
  | 'inject_hint';

export interface MissionSignalRequest {
  signal_type: MissionSignalType;
  content: string;
  branch_id?: string | null;
  created_by?: string;
  metadata?: Record<string, unknown>;
}

export interface MissionSignalResponse {
  accepted: boolean;
  signal_type: MissionSignalType;
  directive_id: string | null;
  mission_id: string;
  note: string | null;
}

export interface ReportResponse {
  id: string;
  status: 'ready' | 'not_ready';
  reason: string | null;
  available_formats: string[];
  payload: Record<string, unknown> | null;
}

export interface AgentNarrativeEvent {
  id: string;
  project_id: string;
  audit_run_id: string | null;
  mission_id: string | null;
  branch_id: string | null;
  task_id: string | null;
  source_agent: string;
  event_kind: "progress" | "reasoning_summary" | "failure_analysis" | "next_action" | "observer_note" | "advisor_note" | "reflector_note" | "worker_summary";
  original_text: string;
  display_text?: string | null;
  original_language: string | null;
  display_language?: string | null;
  model_invocation_id?: string | null;
  created_at: string;
  metadata: Record<string, unknown>;
}

export interface CreateAgentNarrativeRequest {
  source_agent: string;
  event_kind: "progress" | "reasoning_summary" | "failure_analysis" | "next_action" | "observer_note" | "advisor_note" | "reflector_note" | "worker_summary";
  original_text: string;
  audit_run_id: string | null;
  mission_id: string | null;
  branch_id: string | null;
  task_id: string | null;
  original_language: string | null;
  metadata: Record<string, unknown>;
}

// ==========================================
// Structured Constraints (Raw Input Guard)
// ==========================================

export interface StructuredConstraintContract {
  in_scope: string[];
  forbidden_targets: string[];
  forbidden_ports: string[];
  forbidden_actions: string[];
  max_intrusiveness: 'passive' | 'active' | 'exploit';
  notes: string[];
}

// ==========================================
// Intelligence Hub (GET/POST /intelligence/*)
// ==========================================

export type IntelQueryType = 'domain' | 'ip' | 'url' | 'certificate' | 'organization' | 'keyword';
export type IntelEntityKind =
  | 'domain'
  | 'ip'
  | 'url'
  | 'service'
  | 'certificate'
  | 'technology'
  | 'fingerprint'
  | 'organization'
  | 'repository'
  | 'application';
export type IntelRelationKind =
  | 'resolves_to'
  | 'historically_resolved_to'
  | 'certificate_contains'
  | 'references'
  | 'same_favicon'
  | 'same_fingerprint'
  | 'same_ip'
  | 'uses_technology'
  | 'served_by'
  | 'possible_environment_of';
export type IntelConfidence = 'weak' | 'medium' | 'high' | 'confirmed';
export type IntelEntityStatus = 'candidate' | 'promoted' | 'rejected';

export interface IntelQuery {
  seed: string;
  query_type: IntelQueryType;
  source_ids?: string[];
  filters?: Record<string, unknown>;
  limit?: number;
  project_id?: string | null;
  mission_id?: string | null;
}

/** 不含 raw payload 的 provenance 摘要。 */
export interface IntelProvenance {
  raw_record_id: string;
  source: string;
  source_record_id: string;
  query: IntelQuery;
  fetched_at: string;
}

export interface IntelEntity {
  id: string;
  kind: IntelEntityKind;
  value: string;
  normalized_value: string;
  confidence: IntelConfidence;
  status: IntelEntityStatus;
  first_seen: string;
  last_seen: string;
  hit_sources: string[];
  source_count: number;
}

/** 逻辑实体 + 完整多来源 provenance（同一实体被多源命中时 provenance 有多条）。 */
export interface IntelEntityRecord extends IntelEntity {
  provenance: IntelProvenance[];
}

/** 逻辑关系 + 完整多来源 provenance。 */
export interface IntelRelationRecord {
  id: string;
  from_entity_id: string;
  relation: IntelRelationKind;
  to_entity_id: string;
  confidence: IntelConfidence;
  hit_sources: string[];
  source_count: number;
  first_seen: string;
  last_seen: string;
  provenance: IntelProvenance[];
}

export interface IntelSourceInfo {
  id: string;
  display_name: string;
  capabilities: {
    query_types: IntelQueryType[];
    filter_keys: string[];
    requires_credentials: boolean;
  };
  implemented: boolean;
}

export interface IntelSourceResultSummary {
  source_id: string;
  record_count: number;
  fetched_at: string;
  errors: string[];
}

export interface IntelExpansionReport {
  query_id: string;
  query: IntelQuery;
  source_results: IntelSourceResultSummary[];
  successful_sources: string[];
  failed_sources: string[];
  partial: boolean;
  warnings: string[];
  fetched_at: string;
  entities: IntelEntityRecord[];
  relations: IntelRelationRecord[];
}

export interface IntelRawRecord {
  id: string;
  source: string;
  source_record_id: string;
  query: IntelQuery;
  raw_payload: unknown;
  fetched_at: string;
}

/** 晋升结果（entity 更新 + 新建/合并的 Mission 资产）。 */
export interface IntelPromoteOutcome {
  entity: IntelEntity;
  asset: MissionAsset;
}

// ---------------------------------------------------------------------------
// External Worker Runtimes（外部执行边界）
// ---------------------------------------------------------------------------

export type WorkerRuntimeType = 'claude_code' | 'codex' | 'pi' | 'deepseek_harness';

export type WorkerAvailability =
  | 'available'
  | 'not_installed'
  | 'unavailable'
  | 'unsupported'
  | 'not_ready'
  | 'error'
  // 前端专用占位态：后端不会下发；用于 Worker 池页先把固定的几个 runtime
  // 渲染出来，`--version` 探测结果异步回填前显示"探测中"。
  | 'probing';

export type WorkerRunStatus =
  | 'pending'
  | 'running'
  | 'succeeded'
  | 'failed'
  | 'timeout'
  | 'cancelled';

export type WorkerEventKind = 'state' | 'output' | 'error' | 'notice';

export type WorkerExecutionEnvironment = 'local' | 'container';

export interface WorkerEvent {
  seq: number;
  kind: WorkerEventKind;
  at: string;
  message: string;
}

/** 探测结论（API/UI 的 Worker Runtime 状态来源；不含任何密钥）。 */
export interface WorkerProbe {
  runtime: WorkerRuntimeType;
  availability: WorkerAvailability;
  version: string | null;
  detail: string | null;
  capabilities: string[];
  checked_at: string;
}

/** 一次外部 worker 会话（受控 observation，绝不直接成为 Finding）。 */
export interface WorkerUsage {
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  reasoning_tokens: number;
  cost_usd: number | null;
  num_turns: number | null;
  duration_api_ms: number | null;
  requested_model: string | null;
}

export interface GatewayModelInfo {
  name: string;
  provider_type: string;
  upstream_base: string;
  model: string;
  api_key_env: string;
}

export interface GatewayAgentBinding {
  alias: string;
  provider: string | null;
}

export interface GatewayStatus {
  enabled: boolean;
  running: boolean;
  port: number | null;
  pid: number | null;
  models: string[];
  agents: Record<string, GatewayAgentBinding>;
  config_path: string | null;
  started_at: string | null;
  last_error: string | null;
}

/** 网关用量多维报表（LiteLLM 统计页数据源；cost 仅真实报出时非 null）。 */
export interface WorkerUsageSummary {
  runs: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  reasoning_tokens: number;
  cost_usd: number | null;
}

export interface WorkerUsageDailyPoint {
  day: string;
  runs: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  cost_usd: number | null;
}

export interface WorkerUsageModelSlice {
  runtime: string;
  model: string | null;
  requested_model: string | null;
  runs: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  cost_usd: number | null;
}

export interface WorkerUsageGroupedDailyPoint {
  day: string;
  /** runtime wire 名或模型别名；未记录模型时为 null。 */
  key: string | null;
  runs: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  cost_usd: number | null;
}

export type WorkerUsageDimension = 'runtime' | 'model';

export interface WorkerUsageBreakdown {
  summary: WorkerUsageSummary;
  daily: WorkerUsageDailyPoint[];
  by_model: WorkerUsageModelSlice[];
  by_runtime: WorkerUsageModelSlice[];
  /** group_by 指定维度时的（日 × 维度键）聚合；未指定为空。 */
  grouped_daily: WorkerUsageGroupedDailyPoint[];
}

export interface WorkerRun {
  id: string;
  project_id: string;
  mission_id: string | null;
  run_id: string | null;
  branch_id: string | null;
  task_id: string | null;
  runtime: WorkerRuntimeType;
  profile_id: string | null;
  connection_id: string | null;
  model: string | null;
  execution_environment: WorkerExecutionEnvironment;
  status: WorkerRunStatus;
  instruction: string;
  session_ref: string | null;
  transcript_path: string | null;
  summary: string | null;
  error: string | null;
  exit_code: number | null;
  events: WorkerEvent[];
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  duration_ms: number | null;
  metadata: Record<string, unknown>;
  usage?: WorkerUsage | null;
}

export interface WorkerInvocation {
  id: string;
  project_id: string | null;
  worker_run_id: string | null;
  runtime: WorkerRuntimeType;
  connection_id: string | null;
  model: string | null;
  purpose: 'probe' | 'start' | 'resume' | 'cancel';
  status: WorkerRunStatus;
  summary: string | null;
  error: string | null;
  exit_code: number | null;
  started_at: string;
  finished_at: string | null;
  duration_ms: number | null;
}

/** runtime → Connection 绑定（复用 Provider 配置；不含密钥）。 */
export interface WorkerRuntimeProfile {
  id: string;
  runtime_type: WorkerRuntimeType;
  connection_id: string;
  model_override: string | null;
  execution_environment: WorkerExecutionEnvironment;
  max_concurrency: number;
  timeout_seconds: number;
  runtime_options: Record<string, unknown>;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface UpsertWorkerRuntimeProfileRequest {
  runtime_type: WorkerRuntimeType;
  connection_id: string;
  model_override?: string | null;
  /** Agent 预设绑定（Some("") 清除；undefined 保持不变）。 */
  agent_preset?: string | null;
  execution_environment?: WorkerExecutionEnvironment;
  max_concurrency?: number;
  timeout_seconds?: number;
  enabled?: boolean;
}

export interface WorkerRunDetail {
  run: WorkerRun;
  invocations: WorkerInvocation[];
}

// ── Dashboard headline cards (GET /stats/dashboard) ──────────────────────

export interface MissionActivityStats {
  running: number;
  paused: number;
  waiting_for_decision: number;
  total: number;
}

export interface ConfirmedFindingStats {
  critical: number;
  high: number;
  medium: number;
  low: number;
  info: number;
  total: number;
}

export interface AssetTypeCount {
  asset_type: string;
  count: number;
}

export interface AssetNodeStats {
  distinct_count: number;
  by_type: AssetTypeCount[];
}

export interface ToolCallStats {
  total_invocations: number;
  active_worker_runs: number;
}

export interface TokenUsageStats {
  /** 0 = 没有任何真实 usage 上报，前端必须显示占位而非 0。 */
  reported_runs: number;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  cost_reported_runs: number;
  cost_usd: number | null;
}

export interface DashboardStats {
  missions: MissionActivityStats;
  confirmed_findings: ConfirmedFindingStats;
  asset_nodes: AssetNodeStats;
  tool_calls: ToolCallStats;
  token_usage: TokenUsageStats;
}

// ── Agent presets（GET/POST /agent-presets）──────────────────────────────

export interface AgentPreset {
  key: string;
  name: string;
  description: string | null;
  builtin: boolean;
  enabled: boolean;
  model_alias: string | null;
  max_turns: number | null;
  instruction_template: string;
  variables: string[];
  wrapup_template: string | null;
  /** Skill 可见性白名单（空 = 全部可见；fail-closed）。 */
  skills: string[];
  /** 工具授权白名单（空 = 不限制；与任务策略取交集）。 */
  tools: string[];
  created_at: string;
  updated_at: string;
}

export interface CreateAgentPresetRequest {
  key: string;
  name: string;
  description?: string | null;
  instruction_template: string;
  wrapup_template?: string | null;
  model_alias?: string | null;
  max_turns?: number | null;
}

export interface UpdateAgentPresetRequest {
  name?: string | null;
  description?: string | null;
  enabled?: boolean | null;
  model_alias?: string | null;
  max_turns?: number | null;
  instruction_template?: string | null;
  wrapup_template?: string | null;
  skills?: string[] | null;
  tools?: string[] | null;
}

export interface AgentPresetPreviewResponse {
  key: string;
  variables: string[];
  rendered: string;
}

// ── Skills（skills/<name>/SKILL.md 目录）────────────────────────────────

export interface SkillMeta {
  name: string;
  description: string;
  license: string | null;
  compatibility: string | null;
  /** 工具目录条目 id：skill_load 时 fail-closed 解锁。 */
  modules: string[];
}

export interface SkillUsageRow {
  ts: string;
  skill: string;
  agent_preset: string | null;
  mission_id: string | null;
  run_id: string | null;
  args_len: number;
  found: boolean;
}

export interface SkillMissingEntry {
  skill: string;
  misses: number;
  last_ts: string;
}

export interface CreateSkillRequest {
  name: string;
  description: string;
  modules?: string[];
  license?: string | null;
  compatibility?: string | null;
  body?: string;
}

// ── 探索链路（GET /missions/{id}/exploration-graph）────────────────────

export type ExploreKind = 'task' | 'begin' | 'goal' | 'intent' | 'fact' | 'finding' | 'hint';
export type ExploreRel = 'spawns' | 'derived_from' | 'yields' | 'proves';

export interface ExplorationNode {
  id: string;
  kind: Exclude<ExploreKind, 'task'>;
  title: string;
  summary: string;
  state: string;
  priority: number;
  origin: string;
  ts: string;
}

export interface ExplorationEdge {
  src: string;
  dst: string;
  rel: ExploreRel;
}

/**
 * 模型调用审计记录（一次 provider 调用的脱敏留痕）。
 *
 * `reasoning` 是模型思考过程原文，与 `response_summary`（答案摘要）分开：
 * 思考常比答案长数倍，后端单独落库、单独有界。
 */
export interface ModelInvocation {
  id: string;
  project_id: string | null;
  run_id: string | null;
  task_id: string | null;
  provider_id: string;
  provider_type: string;
  model: string | null;
  /** 模型分工用途：natural_language_intake / agent_tool_harness / strategy_board_maintainer / metacognition_divergence。 */
  purpose: string;
  prompt_summary: string;
  response_summary: string;
  prompt_hash: string | null;
  response_hash: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
  status: 'ok' | 'error' | 'timeout' | 'denied';
  error: string | null;
  duration_ms: number | null;
  artifact_paths: string[];
  /** 思考过程原文（非推理模型 / 未开 extended thinking 时为 null）。 */
  reasoning: string | null;
  started_at: string;
  finished_at: string | null;
}

export interface ExplorationGraph {
  nodes: ExplorationNode[];
  edges: ExplorationEdge[];
  /**
   * 模型调用留痕（后端按 project 全量返回，未按 mission 过滤）。
   *
   * 与 `nodes` / `edges` 同为 `/missions/{id}/graph` 响应的一部分——
   * 此前 types.ts 漏了它，会话面板因此拿不到思考过程。
   */
  model_invocations: ModelInvocation[];
}
