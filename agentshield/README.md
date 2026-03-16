# AgentShield

A governance control plane for AI agents. AgentShield sits between your AI agents
and execution targets, evaluating every action against live policies before allowing
execution.

**Safe actions auto-execute. Risky actions require approval. Dangerous actions are denied.**

Policy changes instantly alter agent behavior — without changing the agent.

## Quick Start

```bash
# Build
go build -o agentshield ./cmd/agentshield/

# Run
./agentshield

# Open dashboard
open http://localhost:8080
```

Or use the demo script:

```bash
./scripts/start_demo.sh
```

## Usage Example

### 1. Submit an action from your agent

```bash
# Safe payment — auto-approved and executed
curl -X POST http://localhost:8080/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "finance-bot",
    "action_type": "send_payment",
    "parameters": {"amount": 20, "currency": "USD", "recipient": "vendor@example.com"}
  }'
```

Response:
```json
{
  "id": "a1b2c3d4-...",
  "status": "executed",
  "decision": "allow",
  "reason": "payment within auto-approve limit",
  "result": {"success": true, "output": "payment of 20.00 USD sent to vendor@example.com (mock)"}
}
```

### 2. Risky action — requires approval

```bash
# Large payment — held for approval
curl -X POST http://localhost:8080/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "finance-bot",
    "action_type": "send_payment",
    "parameters": {"amount": 5000, "currency": "USD", "recipient": "supplier@example.com"}
  }'
```

Response:
```json
{
  "id": "e5f6g7h8-...",
  "status": "pending",
  "decision": "pending",
  "reason": "payment amount exceeds auto-approve limit"
}
```

Approve it via API or dashboard:
```bash
curl -X POST http://localhost:8080/api/v1/actions/e5f6g7h8-.../approve
```

### 3. Dangerous action — denied

```bash
curl -X POST http://localhost:8080/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "devops-bot",
    "action_type": "delete_database",
    "parameters": {"database": "production"}
  }'
```

Response:
```json
{
  "id": "i9j0k1l2-...",
  "status": "denied",
  "decision": "deny",
  "reason": "action is in denied list"
}
```

### 4. Change policy live

```bash
# Lower the auto-approve limit from $100 to $10
curl -X PATCH http://localhost:8080/api/v1/policy \
  -H "Content-Type: application/json" \
  -d '{"auto_approve_payment_limit": 10}'

# Now a $20 payment requires approval!
curl -X POST http://localhost:8080/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "finance-bot",
    "action_type": "send_payment",
    "parameters": {"amount": 20, "currency": "USD"}
  }'
# → status: "pending" (was "executed" before the policy change)
```

## Demo Agents

Run the included Python scripts to see the full demo:

```bash
# Terminal 1: Start server
./scripts/start_demo.sh

# Terminal 2: Run demo agents
python3 examples/finance_agent.py    # Safe + risky payments
python3 examples/devops_agent.py     # Dangerous action denied
python3 examples/policy_demo.py      # Live policy change demo
```

## API Reference

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/v1/actions` | POST | Submit action for evaluation |
| `/api/v1/actions` | GET | List actions (?status=pending) |
| `/api/v1/actions/{id}` | GET | Get action details |
| `/api/v1/actions/{id}/approve` | POST | Approve pending action |
| `/api/v1/actions/{id}/reject` | POST | Reject pending action |
| `/api/v1/policy` | GET | Get current policy |
| `/api/v1/policy` | PATCH | Update policy (immediate effect) |
| `/api/v1/reset` | POST | Reset all actions and policy |
| `/api/v1/health` | GET | Health check |

## Project Structure

```
agentshield/
├── cmd/agentshield/     # Entry point + embedded dashboard
├── internal/
│   ├── api/             # HTTP handlers + middleware
│   ├── model/           # Data models
│   ├── policy/          # Policy engine with live reload
│   ├── store/           # SQLite persistence
│   └── adapter/         # Execution adapters (payment, admin)
├── examples/            # Demo agent scripts (Python)
├── scripts/             # Demo helper scripts
└── docs/                # Design docs
```

## Documentation

- [System Design](docs/DESIGN.md) — architecture, data model, flow
- [Design Choices](docs/DESIGN_CHOICES.md) — why Go, why SQLite, market research
- [Post-MVP Roadmap](docs/TODO.md) — what comes after the MVP

## License

MIT
