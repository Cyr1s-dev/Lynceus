import { createRootRoute, createRoute, createRouter } from '@tanstack/react-router';
import { AppShell } from '@/ui/untitled/layouts/AppShell';
import {
  LazyAgentsPage,
  LazyAuditRunsPage,
  LazySkillsPage,
  LazyDashboardPage,
  LazyFindingsInboxPage,
  LazyMissionsPage,
  LazyEngineCatalogPage,
  LazySettingsPage,
  LazyToolInvocationsPage,
  LazyKnowledgeBasePage,
  LazyIntelligenceHubPage,
  LazyWorkerRuntimesPage,
  LazyGatewayPage,
} from './lazy-pages';

const rootRoute = createRootRoute({
  component: AppShell,
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: LazyDashboardPage,
});

const inboxRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/inbox',
  component: LazyFindingsInboxPage,
});

const runsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/runs',
  component: LazyAuditRunsPage,
});

const toolsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/tools',
  component: LazyToolInvocationsPage,
});

const modulesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/modules',
  component: LazyEngineCatalogPage,
});

const knowledgeBaseRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/knowledge',
  component: LazyKnowledgeBasePage,
});

const missionsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/missions',
  component: LazyMissionsPage,
});

const missionDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/missions/$missionId',
  component: LazyMissionsPage,
});

const intelligenceHubRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/intelligence',
  component: LazyIntelligenceHubPage,
});

const workerRuntimesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/worker-runtimes',
  component: LazyWorkerRuntimesPage,
});

const gatewayRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/gateway',
  component: LazyGatewayPage,
});

const agentsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/agents',
  component: LazyAgentsPage,
});

const skillsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/skills',
  component: LazySkillsPage,
});

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  component: LazySettingsPage,
});

const routeTree = rootRoute.addChildren([
  indexRoute,
  missionsRoute,
  missionDetailRoute,
  inboxRoute,
  runsRoute,
  toolsRoute,
  modulesRoute,
  knowledgeBaseRoute,
  intelligenceHubRoute,
  workerRuntimesRoute,
  gatewayRoute,
  agentsRoute,
  skillsRoute,
  settingsRoute,
]);

export const router = createRouter({ routeTree });

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
