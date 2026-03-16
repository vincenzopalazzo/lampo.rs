# AgentShield — Design Choices

## Language: Go

### Why Go over alternatives

| Criteria | Go | Python | Rust | TypeScript |
|----------|-----|--------|------|-----------|
| Single binary deploy | Yes | No (venv) | Yes | No (Node) |
| HTTP server ecosystem | Excellent (stdlib) | Good (FastAPI) | Good (axum) | Good (Express) |
| Compile time | ~2s | N/A | ~30s+ | N/A |
| Concurrency | Goroutines | asyncio | tokio | Event loop |
| DevOps adoption | Very high | High | Growing | Medium |
| SQLite support | go-sqlite3 (CGO) | sqlite3 (stdlib) | rusqlite | better-sqlite3 |
| Learning curve | Low | Low | High | Low |

**Go was chosen because:**

1. **Single binary deployment** — the entire application (server + dashboard + SQLite)
   ships as one binary. No runtime, no node_modules, no virtualenv. This is critical
   for a tool that targets DevOps/platform teams.

2. **Infrastructure credibility** — Go is the language of Kubernetes, Docker, Terraform,
   and Prometheus. An agent governance tool written in Go signals "production infrastructure"
   to the target audience.

3. **Performance for middleware** — as a request-path middleware, AgentShield must add
   minimal latency. Go's goroutine-per-request model handles this naturally.

4. **Standard library HTTP** — Go 1.22+ includes method-based routing (`POST /api/v1/actions`)
   in the standard library. No framework dependency needed.

### Why not the original spec's Python/FastAPI

The MVP spec recommended FastAPI. We diverged because:
- The MVP must feel like infrastructure, not a Python script
- Single-binary deployment simplifies the demo significantly
- Go's type system catches integration bugs at compile time
- The adapter pattern maps naturally to Go interfaces

### Market Research Summary

**Competitors in this space (as of 2026):**

- **Microsoft Agent Governance Toolkit** — enterprise-grade, OWASP-focused, deeply
  integrated with Azure/M365. Open-source but heavy.
- **LangChain Guardrails/Middleware** — Python-native, tightly coupled to LangChain
  agents. Not framework-agnostic.
- **GitHub Enterprise AI Controls** — control plane for GitHub Copilot agents.
  Enterprise-only, not general-purpose.
- **Permit.io + MCP** — authorization layer with AI agent support. SaaS-dependent.
- **Guardrails AI** — focused on LLM output validation, not action-level governance.

**AgentShield's differentiator:** framework-agnostic, self-hosted, policy-as-data
(not policy-as-code), and designed for the "control plane" tier between agents and
execution — not inside the agent framework.

## Architecture: API Gateway Pattern

AgentShield uses the **API gateway/proxy** pattern rather than:
- **SDK-based** (requires agent code changes for each language)
- **Sidecar proxy** (adds deployment complexity)
- **In-process middleware** (framework-coupled)

The gateway pattern means any agent that can make HTTP calls can use AgentShield,
regardless of language or framework.

## Storage: SQLite

SQLite was chosen over Postgres/MySQL because:
- Zero configuration — the database is a single file
- WAL mode provides good concurrent read performance
- Embeds directly in the Go binary via CGO
- Perfect for single-node MVP; can migrate to Postgres later
- Demo resets are just `rm agentshield.db`

## Dashboard: Embedded SPA

The dashboard is a single HTML file embedded in the Go binary via `go:embed`.
No build step, no npm, no separate frontend server.

For the MVP, this trades off:
- Rich UI framework features (React, Vue) for zero build complexity
- Component reusability for deployment simplicity
- Type safety for immediate iterability

The dashboard uses vanilla JavaScript and CSS custom properties for theming.
It auto-refreshes every 3 seconds via polling (WebSocket upgrade is a post-MVP item).

## Policy Model: Data-driven, not code-driven

Policies are stored as structured data (JSON in SQLite), not as code (OPA/Rego,
Cedar, etc.). This means:
- Policy changes are a simple PATCH request
- No policy language to learn
- Dashboard can edit policies directly
- Trade-off: less expressive than a full policy language

Post-MVP, a policy DSL or integration with OPA/Cedar can be added as the
policy model grows.

## What's NOT in the MVP

These are explicitly deferred per the spec:
- Bitcoin anchoring / Liquid integration
- Multisig approval workflows
- Decentralized time enforcement
- Complex policy language (OPA/Rego/Cedar)
- Multi-tenant SaaS mode
- Enterprise hardening (auth, RBAC, rate limiting)
- WebSocket real-time updates (polling is sufficient for demo)
- Agent SDK packages (agents use raw HTTP for now)
