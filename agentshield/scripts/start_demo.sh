#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== AgentShield Demo Setup ==="
echo ""

# Build
echo "[1/3] Building AgentShield..."
cd "$ROOT_DIR"
go build -o agentshield ./cmd/agentshield/
echo "      Built successfully."

# Reset DB
echo "[2/3] Cleaning database..."
rm -f agentshield.db
echo "      Database cleaned."

# Start server
echo "[3/3] Starting server on :8080..."
echo ""
echo "  Dashboard: http://localhost:8080"
echo "  API:       http://localhost:8080/api/v1"
echo ""
echo "  Run the demo agents in another terminal:"
echo "    python3 examples/finance_agent.py"
echo "    python3 examples/devops_agent.py"
echo "    python3 examples/policy_demo.py"
echo ""
echo "  Press Ctrl+C to stop."
echo ""

./agentshield -port 8080
