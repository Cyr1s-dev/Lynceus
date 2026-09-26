# Lynceus Streaming Protocol Contract (Phase 0 Freeze)

Frozen alongside `contracts/openapi.json`. These protocols cannot be
expressed in OpenAPI 3.1, so their wire format is specified here. Any change
to a frame field name or event type is a **contract break** and must bump this
document with an explicit migration note.

The AuditEvent payload referenced below is the JSON serialization of the
Rust `AuditEvent` model (schema id `AuditEvent` in `openapi.json`).

---

## 1. SSE — Project Audit Event Stream

**Endpoint:** `GET /projects/{project_id}/audit/events/stream`

**Query parameters:**

| name      | type   | default | notes                                    |
|-----------|--------|---------|------------------------------------------|
| `run_id`  | string | —       | filter events to one run                 |
| `after_id`| string | —       | resume: only events after this event id  |

**Response:** `text/event-stream`, charset UTF-8. Headers:
`Cache-Control: no-cache`, `Connection: keep-alive`, `X-Accel-Buffering: no`.

**Error:** 404 with `{"detail": "..."}` if the project does not exist
(standard Axum API error envelope, checked before the stream opens).

**Event frames** (server polls the repository at ~1s):

```
event: audit_event
data: <AuditEvent JSON — one compact line>

event: heartbeat
data: {}
```

- `audit_event`: fired for each new persisted `AuditEvent`, in id order.
  `data` is the full AuditEvent JSON object (id, project_id, run_id,
  task_id, tool_invocation_id, type, actor, title, message, severity,
  status, data, created_at).
- `heartbeat`: fired when no new events are available. Payload is always
  the empty object `{}`.
- The stream closes when the client disconnects. There is no server-side
  terminal frame; clients must treat disconnect + `after_id` resume as the
  restart protocol.

`AuditEventType` values (closed set):
`project_created`, `run_started`, `run_completed`, `run_failed`,
`run_paused`, `run_waiting_for_decision`, `run_resumed`, `run_cancelled`,
`decision_gate_created`, `decision_gate_answered`,
`decision_gate_cancelled`, `decision_gate_expired`, `task_created`,
`task_started`, `task_succeeded`, `task_failed`, `solver_started`,
`solver_completed`, `solver_failed`, `tool_invocation_completed`,
`tool_invocation_waiting_for_confirmation`, `tool_invocation_failed`,
`asset_discovered`, `evidence_added`, `finding_added`, `observer_reviewed`,
`sarif_exported`, `user_note`.

---

## 2. WebSocket — Mission Channel

**Endpoint:** `WS /ws/missions/{mission_id}`

**Query parameters:** `after_event_id` (string, optional) — resume: replay
persisted events after this id.

**Transport:** single JSON object per text frame; every frame carries an
`event` discriminator. The client sends **no** frames (the server drains and
ignores anything but disconnect).

### 2.1 Server frames

#### `snapshot` — sent immediately after accept

```json
{
  "event": "snapshot",
  "mission_id": "mission_...",
  "project_id": "proj_...",
  "status": "running",
  "active_run_id": "run_... | null",
  "user_goal": "<raw user goal>",
  "timestamp": "<mission.updated_at ISO-8601>"
}
```

#### `replay` — one frame per persisted event the client missed
(scope: `mission.active_run_id`, ordered, bounded by the replay limit)

```json
{
  "event": "replay",
  "mission_id": "mission_...",
  "event_id": "event_...",
  "type": "<AuditEventType value>",
  "title": "...",
  "message": "... | null",
  "severity": "... | null",
  "status": "... | null",
  "data": {},
  "created_at": "<ISO-8601>"
}
```

#### `ready` — after the last replay frame

```json
{
  "event": "ready",
  "mission_id": "mission_...",
  "replayed": 0,
  "heartbeat_seconds": 3.0
}
```

#### `notification` — live push from the Notification Hub

```json
{
  "event": "notification",
  "mission_id": "mission_...",
  "id": "notify_...",
  "kind": "<NotificationKind value>",
  "project_id": "proj_...",
  "mission_id": "mission_... | null",
  "run_id": "run_... | null",
  "branch_id": "branch_... | null",
  "title": "...",
  "message": "... | null",
  "severity": "... | null",
  "requires_action": false,
  "data": {},
  "created_at": "<ISO-8601>"
}
```

Note: the notification payload is flattened over the frame (its own
`mission_id` key overrides the routing wrapper key).

`NotificationKind` values (closed set):
`snapshot`, `mission_event`, `status_changed`, `narrative`, `finding`,
`high_risk`, `decision_required`, `task_failed`, `completed`, `failed`.

#### `gap` — client was too slow, oldest notifications were dropped

```json
{
  "event": "gap",
  "mission_id": "mission_...",
  "dropped": 2,
  "reason": "client too slow; oldest notifications discarded"
}
```

Clients receiving `gap` should refetch state via REST (`GET /missions/{id}`,
`GET /missions/{id}/timeline`) instead of assuming the stream is complete.

#### `heartbeat` — idle keep-alive (~every 3s)

```json
{
  "event": "heartbeat",
  "mission_id": "mission_...",
  "status": "running",
  "active_run_id": "run_... | null"
}
```

#### `error` — terminal frame

```json
{
  "event": "error",
  "mission_id": "mission_...",
  "reason": "mission not found"
}
```

### 2.2 Close codes

| code | meaning                                   |
|------|-------------------------------------------|
| 4404 | mission not found (after an `error` frame)|
| 1011 | internal error (after an `error` frame)   |

### 2.3 Ordering guarantees

1. `snapshot` always arrives first, `ready` always after all `replay` frames.
2. The hub subscription opens **before** the replay, so no notification
   produced during replay is lost; replayed event ids are then suppressed
   from the live stream.
3. Live `notification` frames are delivered in hub order per project topic;
   drop-oldest under backpressure is reported via `gap`.
