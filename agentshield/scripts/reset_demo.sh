#!/usr/bin/env bash
set -e

BASE_URL="${AGENTSHIELD_URL:-http://localhost:8080}"

echo "=== Resetting AgentShield Demo ==="
curl -s -X POST "$BASE_URL/api/v1/reset" | python3 -m json.tool 2>/dev/null || echo "Reset request sent."
echo ""
echo "Demo reset complete. All actions cleared, policy restored to defaults."
