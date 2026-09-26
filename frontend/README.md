# Lynceus Frontend

Lynceus 的 React / TypeScript 前端。

前端只连接真实 API。它不包含 Mock/demo 数据、伪造漏洞、伪造审计执行或伪造 MCP/module 状态。后端未运行时，界面必须显示连接错误或 API 错误，不能编造状态。

## 技术栈

- React 19
- TypeScript
- Vite
- TanStack Query
- TanStack Router
- Tailwind CSS
- shadcn/radix-style UI primitives
- i18next / react-i18next
- Axios API client

## 本地运行

先启动后端：

```powershell
cd D:\Dev\Projects\Lynceus
$env:LYNCEUS_DB="./data/lynceus.db"
$env:LYNCEUS_BIND="127.0.0.1:8000"
cargo run -p api
```

启动前端：

```powershell
cd D:\Dev\Projects\Lynceus\frontend
npm install
npm run dev
```

打开 Vite 地址，通常是：

```text
http://127.0.0.1:5173
```

## 配置

`VITE_API_BASE_URL` is the default backend URL:

```env
VITE_API_BASE_URL=http://127.0.0.1:8000
```

The Settings page can override this at runtime through browser `localStorage`. That override takes priority over `.env`, so if requests go to the wrong API:

1. open **System Settings / API Endpoint**;
2. set the correct API Base URL;
3. test the connection;
4. save and reload.

## 当前覆盖范围

- Security Overview dashboard
- Asset/project list and project detail
- Real audit execution start
- Semgrep scan start for source/web projects
- Audit event timeline via SSE:
  - `GET /projects/{project_id}/audit/events/stream`
- Findings triage view
- Audit execution list
- Tool invocation audit trail
- Provider Runtime management:
  - list/create/update/delete providers
  - backend-secret masking
  - provider health check
- Engine / Module 管理：
  - list/create/update/delete modules
  - module health check
  - 引擎能力查看
  - AI-assisted MCP discovery/proposal/approval flow
- API endpoint setting
- Chinese/English UI switching

## Important behavior

### No Mock mode

项目中刻意没有 `VITE_USE_MOCK`、`mocks.ts` 或前端合成 fallback。空页面表示后端返回空数据；错误表示后端请求失败。

这是有意设计。Lynceus 是证据链审计平台，Web App 不能展示虚假漏洞或虚假工具状态。

### Provider secrets

The browser may submit a raw `api_key` to the backend when creating/updating a provider, but public API responses never return it. Existing keys are displayed only as `has_api_key=true`.

Recommended production-style setup is to use `api_key_ref`, for example:

```text
env:OPENAI_API_KEY
```

### MCP modules

The module wizard does not let the frontend invoke arbitrary MCP tools. It only:

1. creates a discovery session;
2. probes MCP `tools/list`;
3. asks the selected provider/model to organize a proposal, or falls back
   deterministically;
4. lets the user review/edit;
5. approves the proposal into a real `ModuleConfig`.

Real tool invocation must happen through backend solvers and must be recorded as `ToolInvocation`.

## Quality gates

```powershell
npm run tsc
npm run lint
npm run build
```

Expected current result: all pass. Vite may warn about a large chunk; this is a build-size warning, not a correctness failure.

## Backend endpoints consumed

Core:

- `GET /health`
- `GET /projects`
- `POST /projects`
- `GET /projects/{id}`
- `POST /projects/{id}/audit/start`
- `GET /projects/{id}/audit/runs`
- `GET /projects/{id}/findings`
- `GET /projects/{id}/tool-invocations`
- `GET /projects/{id}/events`
- `GET /projects/{id}/audit/events/stream`
- `GET /projects/{id}/sarif`
- `GET /tool-invocations`

Providers:

- `GET /providers`
- `POST /providers`
- `GET /providers/default`
- `GET /providers/{id}`
- `PATCH /providers/{id}`
- `DELETE /providers/{id}`
- `POST /providers/{id}/test`

Modules:

- `GET /modules`
- `POST /modules`
- `GET /modules/{id}`
- `PATCH /modules/{id}`
- `DELETE /modules/{id}`
- `POST /modules/{id}/test`
- `GET /modules/{id}/capabilities`
- `POST /modules/discovery`
- `POST /modules/discovery/{id}/probe`
- `POST /modules/discovery/{id}/propose`
- `GET /modules/discovery/{id}`
- `GET /modules/proposals/{id}`
- `PATCH /modules/proposals/{id}`
- `POST /modules/proposals/{id}/approve`

## Development notes

- Keep route paths stable unless the backend API changes.
- Prefer rendering backend state exactly as returned; do not synthesize security
  findings, run status, provider health, or module health.
- Keep API calls in `src/lib/api.ts`.
- Keep backend/public type mirrors in `src/lib/types.ts` and
  `src/lib/provider-types.ts`.
- Keep user-facing labels in `src/i18n/resources`.
- For production, add authentication before exposing provider/module management
  beyond localhost.
