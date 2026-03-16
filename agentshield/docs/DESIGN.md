# AgentShield — System Design

## Overview

AgentShield is a governance control plane for AI agents. It sits between AI agents
and execution targets (payment systems, infrastructure APIs, databases), evaluating
every proposed action against a live policy before allowing execution.

```
AI Agent
    │
    ▼
┌──────────────────────────────────┐
│         AgentShield              │
│                                  │
│  ┌──────────┐  ┌──────────────┐ │
│  │  Policy   │  │   Adapter    │ │
│  │  Engine   │  │   Registry   │ │
│  └──────────┘  └──────────────┘ │
│  ┌──────────┐  ┌──────────────┐ │
│  │  SQLite   │  │  REST API    │ │
│  │  Store    │  │  + Dashboard │ │
│  └──────────┘  └──────────────┘ │
└──────────────────────────────────┘
    │                    │
    ▼                    ▼
Payment Provider    Admin/Infra API
```

## Core Flow

1. **Agent submits action** via `POST /api/v1/actions`
2. **Policy engine evaluates** the action against current rules
3. **Decision is made:**
   - `allow` → action is auto-executed via the appropriate adapter
   - `deny` → action is blocked, agent receives denial
   - `pending` → action waits for human approval via dashboard
4. **If pending**, a human approves/rejects via the dashboard
5. **Result is returned** to the agent (or stored for polling)

## Components

### Policy Engine (`internal/policy/engine.go`)

The policy engine is the core of AgentShield. It maintains an in-memory cache
of the active policy for fast evaluation, backed by persistent storage.

Policy evaluation order:
1. Check if action type is in the denied list → DENY
2. Check if action type requires approval → PENDING
3. For `send_payment`: compare amount against auto-approve threshold
4. Default: ALLOW

Key property: **Policies can be changed at runtime.** When a policy is updated
via the API, the engine immediately uses the new rules for all future actions.
Already-evaluated actions are not re-evaluated.

### Adapter Registry (`internal/adapter/`)

Adapters are pluggable execution backends. Each adapter maps an action type to
a concrete execution (mock for MVP, real integrations later).

Current adapters:
- **PaymentAdapter**: simulates payment execution
- **AdminAdapter**: simulates infrastructure operations

### Store (`internal/store/sqlite.go`)

SQLite-backed persistence with WAL mode for concurrent reads. Stores:
- Action requests with full lifecycle state
- Policy configuration (single-row table)

### REST API (`internal/api/handler.go`)

Standard HTTP API using Go 1.22+ routing patterns:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/v1/actions` | POST | Submit an action for evaluation |
| `/api/v1/actions` | GET | List actions (filter by ?status=) |
| `/api/v1/actions/{id}` | GET | Get action details |
| `/api/v1/actions/{id}/approve` | POST | Approve a pending action |
| `/api/v1/actions/{id}/reject` | POST | Reject a pending action |
| `/api/v1/policy` | GET | Get current policy |
| `/api/v1/policy` | PATCH | Update policy (live reload) |
| `/api/v1/reset` | POST | Reset demo state |
| `/api/v1/health` | GET | Health check |

### Dashboard (`web/static/index.html`)

Embedded single-page application served by the Go binary. Three views:
- **Pending**: shows actions waiting for approval, with approve/reject buttons
- **History**: table of all processed actions
- **Policy**: live editor for policy values

Auto-refreshes every 3 seconds.

## Data Model

### ActionRequest

```json
{
  "id": "uuid",
  "agent_id": "finance-agent-01",
  "action_type": "send_payment",
  "parameters": {"amount": 5000, "currency": "USD"},
  "status": "pending",
  "decision": "pending",
  "reason": "payment amount exceeds auto-approve limit",
  "result": null,
  "created_at": "2024-01-01T00:00:00Z",
  "updated_at": "2024-01-01T00:00:00Z"
}
```

### PolicyConfig

```json
{
  "auto_approve_payment_limit": 100,
  "denied_actions": ["delete_database"],
  "approval_required_actions": [],
  "updated_at": "2024-01-01T00:00:00Z"
}
```

## Demo Scenarios

| # | Action | Expected Result |
|---|--------|-----------------|
| 1 | $20 payment | Auto-approved and executed |
| 2 | $5000 payment | Pending — requires human approval |
| 3 | delete_database | Denied immediately |
| 4 | Change limit $100→$10, then $20 payment | Now pending (was auto-approved before) |

Scenario 4 is the key investor demonstration — it proves AgentShield is a
governance control plane, not just a rule checker.
