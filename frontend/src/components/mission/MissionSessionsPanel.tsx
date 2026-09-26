/**
 * 任务会话视图 —— 按四条泳道组织一次任务执行的全部会话记录。
 *
 *
 * 四条泳道：主 Agent(User) / 规划 Planner(Brain) /
 * 系统审计(History) / Workers(Radio)。左侧会话列表按泳道分组，右侧为所选
 * 会话的记录流（transcript）。
 *
 * 适配点（我们的后端没有 sessions/SSE 端点，按既有数据推导）：
 * - 会话全部由现有数据客户端聚合：operation-log（SwarmOperationRecord）、
 *   agent-narratives（AgentNarrativeEvent）、exploration-graph（意图节点）；
 * - 泳道归类：role=mgr→主 Agent；advisor/strategy/planner→规划 Planner；
 *   role=obs/rev（observer/reflector）→系统审计；role=sol→Workers；
 * - Worker 会话 = 一个意图一个会话（exploration-graph 的 intent 节点提供
 *   标题与状态），操作记录经 payload.observation.intent_id 归并到意图，
 *   无法归并的落入「未关联意图」；
 * - 状态机沿用既定图标语义：执行中/待领取/完成/已停止/出错/步数耗尽。
 *
 * 交互补齐（全部落到我们自己的后端端点上）：
 * - 主 Agent 会话底部有消息框：Enter 下发用户指令（POST
 *   /missions/{id}/directives，add_requirement 会提升运行复杂度，主 Agent
 *   下一步纳入），已发消息以「操作员」气泡即时回显；
 * - 右上角「旁路提问」面板 = 旁路提问工作区：另起只读顾问
 *   worker 读任务白板作答（POST /missions/{id}/advise），不打断主 Agent；
 *   主 Agent 消息框里以 /btw 开头同样走这条侧信道。
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import {
  Brain,
  CircleCheck,
  CircleSlash,
  CircleX,
  Clock,
  History,
  Loader2,
  Pause,
  Radio,
  SendHorizontal,
  Sparkles,
  User,
  ZapOff,
} from 'lucide-react';

import { api, getApiErrorMessage } from '@/lib/api';
import { workerHarnessName } from '@/lib/provider-types';
import type {
  AgentNarrativeEvent,
  ModelInvocation,
  SwarmOperationRecord,
  WorkerRunStatus,
} from '@/lib/types';
import { cn } from '@/lib/utils';
import { Badge, Button, EmptyState, ErrorState, Textarea } from '@/ui/untitled';
import { useToast } from '@/hooks/use-toast';

type LaneRole = 'mainagent' | 'planner' | 'system' | 'worker';

type SessionStatus = 'running' | 'pending' | 'done' | 'stopped' | 'blocked' | 'exhausted' | 'paused';

interface SessionEntry {
  id: string;
  ts: string;
  actor: string;
  kind: string;
  kindLabel: string;
  text: string;
  payload?: unknown;
}

interface SessionView {
  key: string;
  lane: LaneRole;
  title: string;
  status: SessionStatus;
  live: boolean;
  lastTs: string;
  entries: SessionEntry[];
  /** 该会话实际由哪个 Harness 执行（worker 会话；未知为 null）。 */
  harness?: string | null;
}

/** 操作员自己下发的指令（本地回显，服务端落库在 user_directives）。 */
interface OperatorTurn {
  id: string;
  ts: string;
  text: string;
  state: 'sending' | 'sent' | 'failed';
}

/** 旁路提问对话轮次（服务端无状态，历史仅在本组件维护）。 */
interface AdvisorTurn {
  role: 'user' | 'assistant';
  content: string;
}

const LANE_ORDER: LaneRole[] = ['mainagent', 'planner', 'system', 'worker'];

const LANE_ICON = {
  mainagent: User,
  planner: Brain,
  system: History,
  worker: Radio,
} as const;

const LANE_CHIP = {
  mainagent: 'bg-blue-500',
  planner: 'bg-violet-500',
  system: 'bg-slate-500',
  worker: 'bg-emerald-500',
} as const;

/** 实时判定窗口：最后一条记录在此窗口内视为活跃。 */
const LIVE_WINDOW_MS = 90_000;

const PLANNER_HINT = /advisor|strategy|planner|planning|termination|decompos/i;
const SYSTEM_HINT = /observer|guardian|reflector|critique|audit/i;

/** 主 Agent 消息框里以 /btw 开头即转走旁路提问。 */
const BTW_PREFIX = '/btw';

const QUICK_ASKS = [
  'sessionsQuickAskProgress',
  'sessionsQuickAskBranches',
  'sessionsQuickAskFailures',
  'sessionsQuickAskEvidence',
] as const;

// narrative event_kind → i18n 键后缀。**必须与 `resources/*.ts` 里的
// `sessionsKind<PascalCase>` 逐字对齐**——早先这里全是小写开头
// （`progress` / `workerSummary`），而资源键是 `sessionsKindProgress` /
// `sessionsKindWorkerSummary`，大小写不匹配让 `t()` 原样返回键名，界面
// 上就出现 "missions.sessionsKindprogress" 这种未翻译原文。
const NARRATIVE_KIND_LABEL: Record<string, string> = {
  progress: 'Progress',
  reasoning_summary: 'Reasoning',
  failure_analysis: 'Failure',
  next_action: 'NextAction',
  observer_note: 'Observer',
  advisor_note: 'Advisor',
  reflector_note: 'Reflector',
  worker_summary: 'WorkerSummary',
};

function StatusIcon({ status }: { status: SessionStatus }) {
  switch (status) {
    case 'running':
      return <Loader2 className="size-3.5 animate-spin text-blue-500" />;
    case 'paused':
      return <Pause className="size-3.5 text-amber-500" />;
    case 'pending':
      return <Clock className="size-3.5 text-muted-foreground" />;
    case 'done':
      return <CircleCheck className="size-3.5 text-emerald-500" />;
    case 'stopped':
      return <CircleSlash className="size-3.5 text-amber-500" />;
    case 'blocked':
      return <CircleX className="size-3.5 text-red-500" />;
    case 'exhausted':
      return <ZapOff className="size-3.5 text-violet-500" />;
  }
}

/** worker-run 状态 → 会话状态。 */
function workerRunStatus(status: WorkerRunStatus): SessionStatus {
  switch (status) {
    case 'succeeded':
      return 'done';
    case 'failed':
    case 'timeout':
      return 'blocked';
    case 'running':
      return 'running';
    case 'pending':
      return 'pending';
    // 取消（暂停/停止 mission 时后端收口）显示为"已停止"而非"待领取"——
    // 后者会让用户以为它还会被拉起，且转圈的是 running，这里本就不该转。
    case 'cancelled':
      return 'stopped';
    default:
      return 'pending';
  }
}

function asString(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value : null;
}

function observationOf(op: SwarmOperationRecord): Record<string, unknown> | null {
  const observation = op.payload?.observation;
  return observation && typeof observation === 'object'
    ? (observation as Record<string, unknown>)
    : null;
}

/** 操作记录泳道：planner 特征优先，其后按后端 role 分类表归类。 */
function laneOfOperation(op: SwarmOperationRecord): LaneRole {
  const haystack = `${op.worker_id ?? ''} ${op.role} ${op.operation_type} ${op.actor_label} ${op.entry}`;
  if (PLANNER_HINT.test(haystack)) return 'planner';
  if (op.role === 'mgr') return 'mainagent';
  if (op.role === 'sol') return 'worker';
  // adv=顾问（规划智囊）；obs/rev=观察/反思（系统审计）。
  if (op.role === 'adv') return 'planner';
  return 'system';
}

/** 叙述事件泳道：顾问→规划，观察/反思→系统审计，带任务→Worker，其余→主 Agent。 */
function laneOfNarrative(n: AgentNarrativeEvent): LaneRole {
  if (n.event_kind === 'advisor_note' || PLANNER_HINT.test(n.source_agent)) return 'planner';
  if (
    n.event_kind === 'observer_note' ||
    n.event_kind === 'reflector_note' ||
    SYSTEM_HINT.test(n.source_agent)
  ) {
    return 'system';
  }
  if (n.task_id) return 'worker';
  return 'mainagent';
}

/**
 * 模型调用目的 → 泳道（无 task_id 时）。
 *
 * 与 Settings 的模型分工用途同源：`natural_language_intake` = 目标拆解 →
 * 规划；`strategy_board_maintainer` / `metacognition_divergence` = 策略板 /
 * 元认知 → 系统审计。未列入的用途归主 Agent（宁可落错泳道，也不为了一张
 * 映射表把新用途吞掉）。
 */
const PURPOSE_LANE: Record<string, LaneRole> = {
  natural_language_intake: 'planner',
  strategy_board_maintainer: 'system',
  metacognition_divergence: 'system',
};

/**
 * 模型调用泳道：带 task_id 的一律归 Worker（分支任务内的调用，含
 * `agent_tool_harness`）；其余按 purpose 归类。
 */
function laneOfInvocation(inv: ModelInvocation): LaneRole {
  if (inv.task_id) return 'worker';
  return PURPOSE_LANE[inv.purpose] ?? 'mainagent';
}

function laneKey(lane: LaneRole): string {
  return lane === 'worker' ? 'unassigned' : lane;
}

function isLive(lastTs: string): boolean {
  if (!lastTs) return false;
  return Date.now() - new Date(lastTs).getTime() < LIVE_WINDOW_MS;
}

export function MissionSessionsPanel({
  missionId,
  projectId,
  className,
}: {
  missionId: string;
  projectId: string;
  className?: string;
}) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const [activeKey, setActiveKey] = useState<string | null>(null);

  // 主 Agent 消息框（用户指令）与旁路提问（只读顾问）状态。
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);
  const [operatorTurns, setOperatorTurns] = useState<OperatorTurn[]>([]);
  const [sideOpen, setSideOpen] = useState(false);
  const [advisorTurns, setAdvisorTurns] = useState<AdvisorTurn[]>([]);
  const [advisorInput, setAdvisorInput] = useState('');
  const [advisorLoading, setAdvisorLoading] = useState(false);
  const [advisorError, setAdvisorError] = useState<string | null>(null);
  const transcriptRef = useRef<HTMLDivElement>(null);
  const advisorScrollRef = useRef<HTMLDivElement>(null);

  const graphQuery = useQuery({
    queryKey: ['exploration-graph', missionId],
    queryFn: () => api.getExplorationGraph(missionId),
    refetchInterval: 20_000,
  });

  // 项目级 GraphSnapshot：model_invocations（含 reasoning）的唯一来源。
  // /missions/{id}/exploration-graph 只投 {nodes, edges}，没有它。
  const projectGraphQuery = useQuery({
    queryKey: ['project-graph', projectId],
    queryFn: () => api.getProjectGraph(projectId),
    refetchInterval: 20_000,
  });

  // Worker Run：task_id → runtime。派发一开始就落了一条 running 骨架
  // （WorkerRegistry::begin_dispatch），所以 worker 还在跑时这里就能
  // 回答"这次是哪个 Harness"，不必等它跑完。
  const workerRunsQuery = useQuery({
    queryKey: ['worker-runs', projectId, 'sessions'],
    queryFn: () => api.listWorkerRuns({ project_id: projectId, limit: 200 }),
    refetchInterval: 5_000,
  });

  const opsQuery = useQuery({
    queryKey: ['mission-operation-log', missionId, 'sessions'],
    queryFn: () => api.getMissionOperationLog(missionId, { after_sequence: 0, limit: 500 }),
    refetchInterval: 20_000,
  });

  const narrativesQuery = useQuery({
    queryKey: ['agent-narratives', projectId, { missionId, scope: 'sessions' }],
    queryFn: () => api.getAgentNarratives(projectId, { mission_id: missionId, limit: 200 }),
    refetchInterval: 20_000,
  });

  const { sessions, laneGroups } = useMemo(() => {
    const ops = opsQuery.data?.records ?? [];
    const narratives = narrativesQuery.data ?? [];
    // 模型调用：项目级 GraphSnapshot 才有 model_invocations（按 project 过滤）。
    // reasoning 非空的才进会话流——没有思考的调用不产生噪音条目。
    const invocations = (projectGraphQuery.data?.model_invocations ?? []).filter(
      (inv) => (inv.reasoning ?? '').trim().length > 0,
    );
    const workerRuns = workerRunsQuery.data ?? [];

    // task_id → Harness 展示名。一个 task 可能被重试多次（多次 worker run），
    // 取最后一次——它就是当前会话归属的那次执行。
    const harnessByTask = new Map<string, string>();
    // task_id → 该 task 的 Worker 会话 key。
    //
    // **这是消除重复行的关键**：worker-run 会话用 `worker-run-<id>` 做键，
    // 而 op/narrative/invocation 只带 task_id。两者键空间不同就会同一次
    // 执行裂成两行——一行挂着完整记录，另一行空着显示"暂无记录"。所有
    // 归到该 task 的记录都必须落到 run 自己那个键上。
    const runKeyByTask = new Map<string, string>();
    for (const run of workerRuns) {
      if (!run.task_id) continue;
      const name = workerHarnessName(run.runtime);
      if (name) harnessByTask.set(run.task_id, name);
      runKeyByTask.set(run.task_id, `worker-run-${run.id}`);
    }

    // task_id → intent_id：GraphSnapshot 的 tasks 是唯一可靠来源（后端
    // `Intent.claimed_by_task_id` 从未被写入，链接只存在于 task 侧）。
    const intentByTask = new Map<string, string>();
    for (const task of projectGraphQuery.data?.tasks ?? []) {
      if (task.intent_id && task.id) intentByTask.set(task.id, task.intent_id);
    }

    // intent_id → 意图标题：只用于给 Worker 行加一句可读后缀。
    // 意图本身**不**在会话面板里占一行——它属于探索画布。
    const intentTitleById = new Map<string, string>();
    for (const intent of projectGraphQuery.data?.intents ?? []) {
      if (intent.id && intent.title) intentTitleById.set(intent.id, intent.title);
    }

    // Harness 归属：先看这条记录自己的 task，再顺 task → intent → task 反查。
    const harnessFor = (taskId: string | null, intentId: string | null): string | undefined => {
      const direct = taskId ? harnessByTask.get(taskId) : undefined;
      if (direct) return direct;
      if (intentId) {
        for (const [tid, iid] of intentByTask) {
          if (iid === intentId) {
            const viaIntent = harnessByTask.get(tid);
            if (viaIntent) return viaIntent;
          }
        }
      }
      return undefined;
    };

    // 把一次记录归到它的 Worker 会话上。优先落到该 task 对应的 worker-run
    // 会话（键一致才不会裂成两行）；run 还没落库 / 记录不带 task_id 时退回
    // task_id 本身或 `unassigned` 桶，由后面的空会话清理兜底。
    const resolveWorkerKey = (explicitIntent: string | null, taskId: string | null): string =>
      (taskId ? runKeyByTask.get(taskId) : undefined) ??
      taskId ??
      explicitIntent ??
      'unassigned';

    // 归属不到 task 的条目（unassigned）用：整个项目只跑过一种 Harness 时，
    // 它就是唯一合理的答案；跑过多种则不猜，退回通用名。判定依据是**去重后
    // 的 Harness 集合大小**，不是 run 条数——同一种 CLI 重试三次仍算一种。
    const distinctHarnesses = new Set(
      workerRuns
        .map((run) => workerHarnessName(run.runtime))
        .filter((name): name is string => Boolean(name)),
    );
    const soleHarness =
      distinctHarnesses.size === 1 ? [...distinctHarnesses][0] : undefined;

    // Worker 行标题：`<Harness> · <意图标题 或 #task 前缀>`。
    //
    // Harness 永远在最前——"这次跑的是 Claude Code 还是 Codex / Pi / DSH"
    // 是第一眼要确认的事。后缀优先用意图标题（可读），没有归属时才退回
    // task 短前缀。刻意**不**再用"未关联意图"这个词：worker-run 天然带
    // runtime，能归属而不归属才是异常，拿它当默认文案会把异常说成常态。
    const workerTitle = (taskId: string | null | undefined): string => {
      const harness =
        (taskId ? harnessByTask.get(taskId) : undefined) ??
        soleHarness ??
        t('missions.sessionsWorkerFallback');
      const intentId = taskId ? intentByTask.get(taskId) : undefined;
      const intentTitle = intentId ? intentTitleById.get(intentId) : undefined;
      const suffix = intentTitle ?? (taskId ? `#${taskId.slice(0, 8)}` : '#unassigned');
      return `${harness} · ${suffix}`;
    };

    const byKey = new Map<string, SessionView>();

    const ensure = (key: string, lane: LaneRole, title: string, status: SessionStatus, live: boolean, lastTs: string, harness?: string | null) => {
      const existing = byKey.get(key);
      if (existing) {
        // 骨架先建（标题还没拿到 harness），后续记录补上归属时回填。
        if (existing.lane === 'worker' && harness && !existing.harness) {
          existing.harness = harness;
        }
        return existing;
      }
      const created: SessionView = { key, lane, title, status, live, lastTs, entries: [], harness };
      byKey.set(key, created);
      return created;
    };

    // 固定泳道会话：主 Agent / 规划 Planner / 系统审计。
    ensure('mainagent', 'mainagent', t('missions.sessionsMainTitle'), 'done', false, '');
    ensure('planner', 'planner', t('missions.sessionsPlannerTitle'), 'done', false, '');
    ensure('system', 'system', t('missions.sessionsSystemTitle'), 'done', false, '');

    // Worker 会话：以 worker-run 为锚，一次执行一行。
    //
    // 意图节点**刻意不进这里**——它属于探索画布（那边才是意图的归属），
    // 而且意图不是 Worker 会话：它没有 runtime，硬塞进来只会得到一行
    // "没有 Harness 名、标题是假设原文"的怪东西，既读不出是哪个 CLI 跑的，
    // 也和画布重复。
    //
    // worker-run 天然带 `runtime`，所以每一行都能回答"这次是 Claude Code /
    // Codex / Pi / DSH"；task_id 再把它接回意图，用于标题后缀。
    for (const run of workerRuns) {
      const taskId = run.task_id;
      // 每个 worker-run 用**自己的引擎**命名：swarm 下同一意图会并发派多个
      // 引擎（各自独立 worker-run、同 task_id），若沿用 harnessByTask 会把它们
      // 全显示成同一个引擎（折叠错觉）。
      const harness = workerHarnessName(run.runtime) ?? t('missions.sessionsWorkerFallback');
      const intentId = taskId ? intentByTask.get(taskId) : undefined;
      const intentTitle = intentId ? intentTitleById.get(intentId) : undefined;
      const title = `${harness} · ${intentTitle ?? (taskId ? `#${taskId.slice(0, 8)}` : '#unassigned')}`;
      ensure(
        `worker-run-${run.id}`,
        'worker',
        title,
        workerRunStatus(run.status),
        run.status === 'running' || run.status === 'pending',
        run.finished_at ?? run.started_at ?? run.created_at,
        harness,
      );
    }

    const pushEntry = (session: SessionView, entry: SessionEntry) => {
      session.entries.push(entry);
      if (!session.lastTs || entry.ts > session.lastTs) session.lastTs = entry.ts;
    };

    for (const op of ops) {
      const lane = laneOfOperation(op);
      const observation = observationOf(op);
      const explicitIntent =
        asString(observation?.intent_id) ??
        asString((observation?.related_intent_ids as unknown[] | undefined)?.[0]);
      const taskId = op.task_id ?? asString(observation?.task_id);
      const key = lane === 'worker' ? resolveWorkerKey(explicitIntent, taskId) : laneKey(lane);
      const session =
        byKey.get(key) ??
        (lane === 'worker'
          ? ensure(key, 'worker', workerTitle(taskId), 'pending', false, '', harnessFor(taskId, explicitIntent))
          : ensure(laneKey(lane), lane, t(`missions.sessions${lane === 'planner' ? 'Planner' : lane === 'system' ? 'System' : 'Main'}Title`), 'done', false, ''));
      pushEntry(session, {
        id: `op-${op.id}`,
        ts: op.created_at,
        actor: op.actor_label || op.worker_id || op.role,
        kind: op.operation_type,
        kindLabel: op.operation_type,
        text: op.entry,
        payload: op.payload,
      });
    }

    for (const n of narratives) {
      const lane = laneOfNarrative(n);
      const key = lane === 'worker' ? resolveWorkerKey(null, n.task_id) : laneKey(lane);
      const session =
        byKey.get(key) ??
        (lane === 'worker'
          ? ensure(key, 'worker', workerTitle(n.task_id), 'pending', false, '', harnessFor(n.task_id, null))
          : ensure(laneKey(lane), lane, t(`missions.sessions${lane === 'planner' ? 'Planner' : lane === 'system' ? 'System' : 'Main'}Title`), 'done', false, ''));
      pushEntry(session, {
        id: `nar-${n.id}`,
        ts: n.created_at,
        actor: n.source_agent,
        kind: n.event_kind,
        kindLabel: t(`missions.sessionsKind${NARRATIVE_KIND_LABEL[n.event_kind] ?? 'progress'}`),
        text: n.display_text || n.original_text,
      });
    }

    // 思考过程：主 Agent / Planner / 系统审计 / Workers 的模型推理原文。
    // 这是"看不到思考过程"的修复面——后端已把三家 wire 的 reasoning 归一化
    // 落到 ModelInvocation.reasoning，这里把它并进同一条会话流。
    for (const inv of invocations) {
      const lane = laneOfInvocation(inv);
      const key = lane === 'worker' ? resolveWorkerKey(null, inv.task_id) : laneKey(lane);
      const session =
        byKey.get(key) ??
        (lane === 'worker'
          ? ensure(key, 'worker', workerTitle(inv.task_id), 'pending', false, '', harnessFor(inv.task_id, null))
          : ensure(laneKey(lane), lane, t(`missions.sessions${lane === 'planner' ? 'Planner' : lane === 'system' ? 'System' : 'Main'}Title`), 'done', false, ''));
      pushEntry(session, {
        id: `inv-${inv.id}`,
        // finished_at 优先：思考属于"这次调用已经结束"的事实；未结束才看 started_at。
        ts: inv.finished_at ?? inv.started_at,
        actor: inv.model || inv.purpose,
        kind: 'reasoning',
        kindLabel: t('missions.sessionsKindThinking'),
        text: inv.reasoning ?? '',
        payload: { purpose: inv.purpose, model: inv.model },
      });
    }

    // Swarm：同一意图并发派多个引擎（各自独立 worker-run、同 task_id）。
    // op/narrative 只带 task_id，会归并到其中一个 run；其余并发 worker 的会话
    // 因此是空的、会被空会话清理删掉——前端就折叠成一行。用每个 worker-run
    // 自己的 summary 兜底填充，让每个引擎都可见。
    for (const run of workerRuns) {
      const summary = (run.summary ?? '').trim();
      if (!summary) continue;
      const session = byKey.get(`worker-run-${run.id}`);
      if (!session || session.entries.length > 0) continue;
      pushEntry(session, {
        id: `run-summary-${run.id}`,
        ts: run.finished_at ?? run.started_at ?? run.created_at,
        actor: workerHarnessName(run.runtime) ?? t('missions.sessionsWorkerFallback'),
        kind: 'worker_summary',
        kindLabel: t('missions.sessionsKindWorkerSummary'),
        text: summary,
      });
    }

    const all = [...byKey.values()];
    for (const session of all) {
      session.entries.sort((a, b) => a.ts.localeCompare(b.ts));
      // 固定泳道：有近期记录即活跃；Worker：run 状态已给出 live。
      if (session.lane !== 'worker' && isLive(session.lastTs)) {
        session.status = 'running';
        session.live = true;
      }
    }
    // 可见性清理（只作用于 Worker 行）：
    // 1. 空白行删除：一条记录都没有、且不在跑的 Worker 行没有信息量（"该会话
    //    暂无记录"），还会把真实执行挤出视野。**在跑的 worker 例外保留**——
    //    swarm 下多个引擎并发跑在同一 task，op/narrative 尚未按引擎分流，但用户
    //    要能立刻看到"这几个引擎都在跑"。典型来源：worker-run 已落库但本次派发
    //    尚未产生任何 op/narrative（刚起步、或派发骨架先于执行记录）。
    // 2. `unassigned` 桶整个丢弃：归不到任何 task/intent 的记录会落进这个
    //    桶，标题退化成 "Worker · #unassigned"——对用户零信息量，明确不展示。
    // 固定泳道（主 Agent / Planner / 系统审计）是常驻入口，两者都不删。
    const visible = all.filter(
      (session) =>
        session.lane !== 'worker' ||
        session.live ||
        (session.entries.length > 0 && session.key !== 'unassigned'),
    );
    // Worker 按最近活跃倒序（最新执行的会话在前）。
    visible.sort((a, b) => {
      const laneDelta = LANE_ORDER.indexOf(a.lane) - LANE_ORDER.indexOf(b.lane);
      if (laneDelta !== 0) return laneDelta;
      return b.lastTs.localeCompare(a.lastTs);
    });

    const groups = new Map<LaneRole, SessionView[]>();
    for (const lane of LANE_ORDER) groups.set(lane, []);
    for (const session of visible) groups.get(session.lane)?.push(session);

    return { sessions: visible, laneGroups: groups };
  }, [opsQuery.data, narrativesQuery.data, graphQuery.data, t]);

  const active =
    sessions.find((s) => s.key === activeKey) ??
    sessions.find((s) => s.live) ??
    sessions.find((s) => s.lane === 'mainagent') ??
    sessions[0] ??
    null;

  /** 旁路提问：另起只读顾问 worker 读白板作答，不打断主 Agent 当前轮次。 */
  const askAdvisor = async (question: string) => {
    const trimmed = question.trim();
    if (!trimmed || advisorLoading) return;
    setAdvisorTurns((prev) => [...prev, { role: 'user', content: trimmed }]);
    setAdvisorInput('');
    setAdvisorLoading(true);
    setAdvisorError(null);
    try {
      const history = advisorTurns.slice(-10).map((turn) => ({ role: turn.role, content: turn.content }));
      const response = await api.missionAdvise(missionId, trimmed, history);
      setAdvisorTurns((prev) => [...prev, { role: 'assistant', content: response.answer }]);
    } catch (error) {
      setAdvisorError(getApiErrorMessage(error));
    } finally {
      setAdvisorLoading(false);
    }
  };

  /** 主 Agent 消息框：/btw 开头转旁路提问，其余作为用户指令下发。 */
  const sendMainMessage = async () => {
    const text = draft.trim();
    if (!text || sending) return;
    if (text.toLowerCase().startsWith(BTW_PREFIX)) {
      const question = text.slice(BTW_PREFIX.length).trim();
      setDraft('');
      if (question) {
        setSideOpen(true);
        await askAdvisor(question);
      }
      return;
    }
    setDraft('');
    setSending(true);
    const turnId = `operator-${Date.now()}`;
    setOperatorTurns((prev) => [...prev, { id: turnId, ts: new Date().toISOString(), text, state: 'sending' }]);
    try {
      await api.applyMissionDirective(missionId, { directive_type: 'add_requirement', content: text });
      setOperatorTurns((prev) => prev.map((turn) => (turn.id === turnId ? { ...turn, state: 'sent' } : turn)));
      queryClient.invalidateQueries({ queryKey: ['mission-canvas', missionId] });
      toast({ title: t('missions.sessionsDirectiveSent') });
    } catch (error) {
      setOperatorTurns((prev) => prev.map((turn) => (turn.id === turnId ? { ...turn, state: 'failed' } : turn)));
      toast({
        title: t('missions.sessionsDirectiveFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    } finally {
      setSending(false);
    }
  };

  const stickToBottom = (container: HTMLElement | null) => {
    if (container) container.scrollTop = container.scrollHeight;
  };

  // 新内容贴底：操作员气泡、顾问回复。只滚各自容器，不牵连整页。
  useEffect(() => {
    stickToBottom(transcriptRef.current);
  }, [operatorTurns, active?.key]);

  useEffect(() => {
    stickToBottom(advisorScrollRef.current);
  }, [advisorTurns, advisorLoading, sideOpen]);

  if (opsQuery.isLoading && graphQuery.isLoading) {
    return (
      <div
        className={cn(
          'flex items-center justify-center rounded-xl border border-border bg-muted/20',
          className,
        )}
      >
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    );
  }
  if (opsQuery.isError && graphQuery.isError) {
    return (
      <ErrorState
        title={t('missions.sessionsEmpty')}
        description={getApiErrorMessage(opsQuery.error ?? graphQuery.error)}
        onRetry={() => {
          void opsQuery.refetch();
          void graphQuery.refetch();
          void narrativesQuery.refetch();
        }}
        retryLabel={t('common.retry')}
        className={className}
      />
    );
  }

  if (sessions.length === 0) {
    return (
      <EmptyState
        title={t('missions.sessionsEmpty')}
        description={t('missions.sessionsEmptyTranscript')}
        className={className}
      />
    );
  }

  return (
    <div
      className={cn(
        'grid gap-4 lg:h-[72vh]',
        sideOpen ? 'lg:grid-cols-[260px_1fr_340px]' : 'lg:grid-cols-[280px_1fr]',
        className,
      )}
    >
      {/* 左：泳道会话列表（会话列表 同款结构） */}
      <div className="flex min-h-0 flex-col rounded-xl border border-border bg-card">
        <div className="min-h-0 flex-1 overflow-y-auto p-2">
          <div className="flex flex-col gap-3">
            {LANE_ORDER.map((lane) => {
              const items = laneGroups.get(lane) ?? [];
              if (items.length === 0) return null;
              const LaneIcon = LANE_ICON[lane];
              return (
                <div key={lane} className="flex flex-col gap-0.5">
                  <div className="flex items-center gap-1.5 px-2 py-1 text-xs font-medium text-muted-foreground">
                    <LaneIcon className="size-3.5" />
                    {t(`missions.sessionsLane${lane === 'mainagent' ? 'Main' : lane === 'planner' ? 'Planner' : lane === 'system' ? 'System' : 'Worker'}`)}
                    <span className="ml-auto tabular-nums">{items.length}</span>
                  </div>
                  {items.map((session) => (
                    <button
                      key={session.key}
                      type="button"
                      onClick={() => setActiveKey(session.key)}
                      className={cn(
                        'flex w-full min-w-0 items-center gap-1.5 rounded-md px-2 py-1.5 text-left transition-colors',
                        active?.key === session.key
                          ? 'bg-accent text-accent-foreground'
                          : 'hover:bg-accent/50',
                      )}
                    >
                      {session.lane === 'worker' ? (
                        <StatusIcon status={session.status} />
                      ) : session.live ? (
                        <Loader2 className="size-3.5 shrink-0 animate-spin text-blue-500" />
                      ) : (
                        <span className="size-3.5 shrink-0" />
                      )}
                      <span className="min-w-0 flex-1 truncate text-sm font-medium">
                        {session.title}
                      </span>
                      {session.live && (
                        <span className="inline-flex shrink-0 items-center gap-1 rounded bg-blue-500/15 px-1.5 py-0.5 text-[10px] font-medium text-blue-600">
                          <span className="size-1 animate-pulse rounded-full bg-blue-500" />
                          {t('missions.sessionsLive')}
                        </span>
                      )}
                    </button>
                  ))}
                </div>
              );
            })}
          </div>
        </div>
      </div>

      {/* 右：会话记录流（会话记录 同款结构） */}
      <div className="flex min-h-0 flex-col overflow-hidden rounded-xl border border-border bg-card">
        {active && (
          <>
            <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 border-b border-border px-3 py-2.5">
              <span
                className={cn(
                  'flex size-6 shrink-0 items-center justify-center rounded-lg',
                  LANE_CHIP[active.lane],
                )}
              >
                {(() => {
                  const LaneIcon = LANE_ICON[active.lane];
                  return <LaneIcon className="size-3.5 text-white" />;
                })()}
              </span>
              <span className="min-w-0 truncate text-sm font-medium">{active.title}</span>
              <span className="flex items-center gap-1 text-xs text-muted-foreground">
                <StatusIcon status={active.status} />
                {t(`missions.sessionsStatus${active.status.charAt(0).toUpperCase()}${active.status.slice(1)}`)}
              </span>
              <Button
                variant={sideOpen ? 'secondary' : 'ghost'}
                size="sm"
                className="ml-1 h-7 gap-1.5 text-xs"
                onClick={() => setSideOpen((open) => !open)}
                aria-pressed={sideOpen}
              >
                <Sparkles className="size-3.5" />
                {t('missions.sessionsSideQuestion')}
              </Button>
              <Badge tone="neutral" size="sm" className="ml-auto tabular-nums">
                {t('missions.sessionsEntryCount', { count: active.entries.length })}
              </Badge>
            </div>
            <div ref={transcriptRef} className="min-h-0 flex-1 overflow-y-auto">
              {active.entries.length === 0 ? (
                <p className="p-4 text-sm text-muted-foreground">
                  {t('missions.sessionsEmptyTranscript')}
                </p>
              ) : (
                <div className="divide-y divide-border">
                  {active.entries.map((entry) => {
                    // Worker 终稿是本次派发要回给用户的结论，视觉上必须从
                    // 进度噪音里跳出来（settlement：这句话就是运行结果）。
                    const isConclusion = entry.kind === 'worker_summary';
                    // 思考过程是模型的内部推理，不是给人读的结论：压暗、
                    // 斜体、左侧细线，与"结论"和"进展"都区分开。
                    const isThinking = entry.kind === 'reasoning';
                    return (
                    <div
                      key={entry.id}
                      className={cn(
                        'flex gap-2.5 px-3 py-2.5',
                        isConclusion && 'border-l-2 border-l-emerald-500 bg-emerald-500/5',
                        isThinking && 'border-l-2 border-l-violet-400/60 bg-violet-500/5',
                      )}
                    >
                      <span
                        className={cn(
                          'mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-md',
                          LANE_CHIP[active.lane],
                        )}
                      >
                        {(() => {
                          const LaneIcon = LANE_ICON[active.lane];
                          return <LaneIcon className="size-3.5 text-white" />;
                        })()}
                      </span>
                      <div className="min-w-0 flex-1">
                        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted-foreground">
                          <span className="font-medium text-foreground">{entry.actor}</span>
                          <Badge tone="neutral" size="sm" className="font-mono text-[10px]">
                            {entry.kindLabel}
                          </Badge>
                          <span className="ml-auto shrink-0 tabular-nums">
                            {new Date(entry.ts).toLocaleString()}
                          </span>
                        </div>
                        <p
                          className={cn(
                            'mt-1 whitespace-pre-wrap break-words text-sm text-foreground',
                            isConclusion && 'font-medium',
                            isThinking && 'text-muted-foreground italic',
                          )}
                        >
                          {entry.text}
                        </p>
                        {entry.payload !== undefined && entry.payload !== null && (
                          <details className="group mt-1">
                            <summary className="cursor-pointer text-xs text-muted-foreground transition-colors hover:text-foreground">
                              {t('missions.sessionsRawPayload')}
                            </summary>
                            <pre className="mt-1 max-w-full overflow-hidden whitespace-pre-wrap break-all rounded-md border bg-muted/50 p-2 font-mono text-xs leading-relaxed text-foreground">
                              {JSON.stringify(entry.payload, null, 2)}
                            </pre>
                          </details>
                        )}
                      </div>
                    </div>
                    );
                  })}
                </div>
              )}
            </div>

            {/* 操作员已下发指令：本地气泡回显（服务端落库 user_directives） */}
            {active.lane === 'mainagent' && operatorTurns.length > 0 && (
              <div className="flex max-h-40 shrink-0 flex-col gap-2 overflow-y-auto border-t border-border px-3 py-2.5">
                {operatorTurns.map((turn) => (
                  <div key={turn.id} className="flex justify-end">
                    <div className="max-w-[85%] rounded-xl rounded-br-sm bg-primary px-2.5 py-1.5 text-xs leading-relaxed text-primary-foreground">
                      <p className="whitespace-pre-wrap break-words">{turn.text}</p>
                      <p className="mt-1 flex items-center justify-end gap-1 text-[10px] opacity-80">
                        {turn.state === 'sending' && <Loader2 className="size-3 animate-spin" />}
                        {turn.state === 'sent'
                          ? t('missions.sessionsTurnSent')
                          : turn.state === 'failed'
                            ? t('missions.sessionsTurnFailed')
                            : t('missions.sessionsTurnSending')}
                      </p>
                    </div>
                  </div>
                ))}
              </div>
            )}

            {/* 主 Agent 消息框：Enter 下发，/btw 转旁路提问 */}
            {active.lane === 'mainagent' && (
              <div className="shrink-0 border-t border-border bg-card p-2.5">
                <div className="flex items-end gap-2 rounded-lg border border-border bg-background px-2 py-1.5">
                  <Textarea
                    rows={1}
                    value={draft}
                    onChange={(event) => setDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' && !event.shiftKey) {
                        event.preventDefault();
                        void sendMainMessage();
                      }
                    }}
                    placeholder={t('missions.sessionsComposerPlaceholder')}
                    aria-label={t('missions.sessionsComposerLabel')}
                    className="max-h-32 min-h-8 flex-1 resize-none border-0 bg-transparent p-0 text-sm shadow-none focus-visible:ring-0"
                  />
                  <Button
                    size="icon"
                    className="size-8 shrink-0"
                    onClick={() => void sendMainMessage()}
                    disabled={sending || !draft.trim()}
                    aria-label={t('missions.sessionsSend')}
                    title={t('missions.sessionsSend')}
                  >
                    {sending ? <Loader2 className="size-4 animate-spin" /> : <SendHorizontal className="size-4" />}
                  </Button>
                </div>
                <p className="mt-1 px-1 text-[11px] leading-relaxed text-muted-foreground">
                  {t('missions.sessionsComposerHint')}
                </p>
              </div>
            )}
          </>
        )}
      </div>

      {/* 旁路提问面板：只读顾问，不打断主 Agent */}
      {sideOpen && (
        <div className="flex min-h-0 flex-col overflow-hidden rounded-xl border border-border bg-card">
          <div className="flex items-center gap-2 border-b border-border px-3 py-2.5">
            <span className="flex size-6 shrink-0 items-center justify-center rounded-lg bg-violet-500">
              <Sparkles className="size-3.5 text-white" />
            </span>
            <span className="min-w-0 truncate text-sm font-medium">{t('missions.sessionsSideTitle')}</span>
            <Button
              variant="ghost"
              size="sm"
              className="ml-auto h-7 text-xs"
              onClick={() => setSideOpen(false)}
            >
              {t('missions.sessionsSideClose')}
            </Button>
          </div>
          <div className="flex flex-wrap gap-1 border-b border-border px-3 py-2">
            {QUICK_ASKS.map((key) => (
              <button
                key={key}
                type="button"
                disabled={advisorLoading}
                onClick={() => void askAdvisor(t(`missions.${key}`))}
                className="rounded-full border border-violet-200 bg-violet-50 px-2 py-0.5 text-[11px] text-violet-700 transition-colors hover:bg-violet-600 hover:text-white disabled:opacity-50"
              >
                {t(`missions.${key}`)}
              </button>
            ))}
          </div>
          <div ref={advisorScrollRef} className="min-h-0 flex-1 space-y-2 overflow-y-auto p-2.5">
            {advisorTurns.length === 0 && !advisorLoading && (
              <p className="px-1 py-2 text-xs leading-relaxed text-muted-foreground">
                {t('missions.sessionsSideHint')}
              </p>
            )}
            {advisorTurns.map((turn, index) => (
              <div key={index} className={cn('flex', turn.role === 'user' ? 'justify-end' : 'justify-start')}>
                <div
                  className={cn(
                    'max-w-[88%] rounded-xl px-2.5 py-1.5 text-xs leading-relaxed whitespace-pre-wrap',
                    turn.role === 'user'
                      ? 'rounded-br-sm bg-violet-600 text-white'
                      : 'rounded-bl-sm border border-border bg-muted/50 text-foreground',
                  )}
                >
                  {turn.content}
                </div>
              </div>
            ))}
            {advisorLoading && (
              <div className="flex justify-start">
                <div className="flex items-center gap-1.5 rounded-xl rounded-bl-sm border border-border bg-muted/50 px-2.5 py-1.5 text-xs text-muted-foreground">
                  <Loader2 className="size-3 animate-spin" />
                  {t('missions.sessionsAdvisorThinking')}
                </div>
              </div>
            )}
            {advisorError && (
              <p className="rounded-lg border border-danger/30 bg-danger-soft px-2.5 py-1.5 text-xs text-danger">
                {advisorError}
              </p>
            )}
          </div>
          <div className="shrink-0 border-t border-border p-2">
            <div className="flex items-end gap-2 rounded-lg border border-border bg-background px-2 py-1.5">
              <Textarea
                rows={1}
                value={advisorInput}
                onChange={(event) => setAdvisorInput(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' && !event.shiftKey) {
                    event.preventDefault();
                    void askAdvisor(advisorInput);
                  }
                }}
                placeholder={t('missions.sessionsAdvisorPlaceholder')}
                aria-label={t('missions.sessionsAdvisorLabel')}
                disabled={advisorLoading}
                className="max-h-28 min-h-8 flex-1 resize-none border-0 bg-transparent p-0 text-sm shadow-none focus-visible:ring-0"
              />
              <Button
                size="icon"
                className="size-8 shrink-0"
                onClick={() => void askAdvisor(advisorInput)}
                disabled={advisorLoading || !advisorInput.trim()}
                aria-label={t('missions.sessionsSend')}
                title={t('missions.sessionsSend')}
              >
                {advisorLoading ? <Loader2 className="size-4 animate-spin" /> : <SendHorizontal className="size-4" />}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
