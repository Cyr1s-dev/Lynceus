/* eslint-disable react-refresh/only-export-components */
import { lazy, Suspense, type ComponentType } from 'react';
import { useTranslation } from 'react-i18next';

const DashboardPage = lazy(() => import('@/pages/DashboardPage').then((module) => ({ default: module.DashboardPage })));
const MissionsPage = lazy(() => import('@/pages/MissionsPage').then((module) => ({ default: module.MissionsPage })));
const FindingsInboxPage = lazy(() => import('@/pages/FindingsInboxPage').then((module) => ({ default: module.FindingsInboxPage })));
const AuditRunsPage = lazy(() => import('@/pages/AuditRunsPage').then((module) => ({ default: module.AuditRunsPage })));
const ToolInvocationsPage = lazy(() => import('@/pages/ToolInvocationsPage').then((module) => ({ default: module.ToolInvocationsPage })));
const SettingsPage = lazy(() => import('@/pages/SettingsPage').then((module) => ({ default: module.SettingsPage })));
const EngineCatalogPage = lazy(() => import('@/pages/EngineCatalogPage').then((module) => ({ default: module.EngineCatalogPage })));
const KnowledgeBasePage = lazy(() => import('@/pages/KnowledgeBasePage').then((module) => ({ default: module.KnowledgeBasePage })));
const IntelligenceHubPage = lazy(() => import('@/pages/IntelligenceHubPage').then((module) => ({ default: module.IntelligenceHubPage })));
const WorkerRuntimesPage = lazy(() => import('@/pages/WorkerRuntimesPage').then((module) => ({ default: module.WorkerRuntimesPage })));
const SkillsPage = lazy(() => import('@/pages/SkillsPage').then((module) => ({ default: module.SkillsPage })));
const AgentsPage = lazy(() => import('@/pages/AgentsPage').then((module) => ({ default: module.AgentsPage })));
const GatewayPage = lazy(() => import('@/pages/GatewayPage').then((module) => ({ default: module.GatewayPage })));

function RouteLoading() {
  const { t } = useTranslation();

  return (
    <div className="p-6 text-sm text-muted-foreground">
      {t('common.loading')}
    </div>
  );
}

function withSuspense(Page: ComponentType) {
  return function LazyRoutePage() {
    return (
      <Suspense fallback={<RouteLoading />}>
        <Page />
      </Suspense>
    );
  };
}

export const LazyDashboardPage = withSuspense(DashboardPage);
export const LazyMissionsPage = withSuspense(MissionsPage);
export const LazyFindingsInboxPage = withSuspense(FindingsInboxPage);
export const LazyAuditRunsPage = withSuspense(AuditRunsPage);
export const LazyToolInvocationsPage = withSuspense(ToolInvocationsPage);
export const LazySettingsPage = withSuspense(SettingsPage);
export const LazyEngineCatalogPage = withSuspense(EngineCatalogPage);
export const LazyKnowledgeBasePage = withSuspense(KnowledgeBasePage);
export const LazyIntelligenceHubPage = withSuspense(IntelligenceHubPage);
export const LazyWorkerRuntimesPage = withSuspense(WorkerRuntimesPage);
export const LazyAgentsPage = withSuspense(AgentsPage);
export const LazySkillsPage = withSuspense(SkillsPage);
export const LazyGatewayPage = withSuspense(GatewayPage);
