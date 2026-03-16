# AgentShield — Post-MVP Roadmap

## Phase 1: MVP (Complete)

- [x] Core backend with policy engine
- [x] SQLite persistence with WAL mode
- [x] REST API (submit, approve, reject, policy CRUD)
- [x] Execution adapters (payment mock, admin mock)
- [x] Embedded web dashboard
- [x] Live policy control (the key feature)
- [x] Demo agent scripts (finance, devops, policy demo)
- [x] Demo helper scripts (start, reset)

## Phase 2: Production Hardening

- [ ] Authentication and API keys for agents
- [ ] RBAC for dashboard users (viewer, approver, admin)
- [ ] Rate limiting per agent
- [ ] Request validation and schema enforcement
- [ ] Structured audit log export (JSON lines)
- [ ] Prometheus metrics endpoint (`/metrics`)
- [ ] Health check with dependency status
- [ ] Graceful shutdown handling
- [ ] Configuration via environment variables / config file

## Phase 3: Real Integrations

- [ ] Stripe adapter (real payment execution in test mode)
- [ ] AWS adapter (EC2, RDS, Lambda operations)
- [ ] Kubernetes adapter (deployment, scaling)
- [ ] Slack/Discord notification adapter (approval requests)
- [ ] Webhook adapter (generic HTTP execution)
- [ ] Email notification for pending approvals

## Phase 4: Advanced Policy

- [ ] Policy DSL or OPA/Rego/Cedar integration
- [ ] Time-based policies (allow only during business hours)
- [ ] Budget policies (daily/weekly spending limits)
- [ ] Agent-specific policy overrides
- [ ] Policy versioning and rollback
- [ ] Policy dry-run mode (evaluate without enforcing)

## Phase 5: Agent SDK

- [ ] Python SDK (`pip install agentshield`)
- [ ] TypeScript/Node SDK (`npm install agentshield`)
- [ ] Go SDK
- [ ] LangChain middleware integration
- [ ] OpenAI function-calling integration
- [ ] MCP (Model Context Protocol) server

## Phase 6: Enterprise Features

- [ ] Multi-tenant SaaS mode
- [ ] SSO / SAML / OIDC authentication
- [ ] Team-based approval workflows
- [ ] Escalation policies (auto-escalate after timeout)
- [ ] Compliance reporting (SOC 2, audit trails)
- [ ] Data retention policies

## Phase 7: Decentralized Features

- [ ] Bitcoin anchoring for immutable audit trails
- [ ] Liquid integration for timestamping
- [ ] Multisig approval (M-of-N approvers)
- [ ] Decentralized time enforcement
- [ ] Cross-organization policy federation

## Phase 8: Intelligence

- [ ] Anomaly detection on agent behavior
- [ ] Risk scoring based on historical patterns
- [ ] LLM-based policy suggestions
- [ ] Agent behavior dashboards and analytics
- [ ] Automated policy tuning
