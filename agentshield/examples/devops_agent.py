#!/usr/bin/env python3
"""DevOps Agent — demonstrates a dangerous action being denied by AgentShield."""

import json
import sys
import requests

BASE_URL = "http://localhost:8080/api/v1"
AGENT_ID = "devops-agent-01"


def submit_action(action_type: str, params: dict) -> dict:
    resp = requests.post(f"{BASE_URL}/actions", json={
        "agent_id": AGENT_ID,
        "action_type": action_type,
        "parameters": params,
    })
    resp.raise_for_status()
    return resp.json()


def print_result(label: str, result: dict):
    status = result["status"]
    decision = result["decision"]
    reason = result.get("reason", "")
    rid = result["id"][:8]

    colors = {
        "executed": "\033[92m",
        "approved": "\033[92m",
        "pending":  "\033[93m",
        "denied":   "\033[91m",
        "failed":   "\033[91m",
    }
    reset = "\033[0m"
    c = colors.get(status, "")

    print(f"\n{'='*60}")
    print(f"  {label}")
    print(f"  ID:       {rid}...")
    print(f"  Decision: {c}{decision}{reset}")
    print(f"  Status:   {c}{status}{reset}")
    print(f"  Reason:   {reason}")
    print(f"{'='*60}")


def main():
    print("\n" + "="*60)
    print("  AgentShield — DevOps Agent Demo")
    print("="*60)

    # Scenario 3: Dangerous action — delete_database
    print("\n[1] Attempting delete_database (should be DENIED)...")
    r = submit_action("delete_database", {
        "database": "production_db",
        "confirm": True,
    })
    print_result("delete_database", r)

    # Scenario: Safe action — restart_service
    print("\n[2] Restarting web service (should auto-approve)...")
    r = submit_action("restart_service", {
        "service": "web-frontend",
    })
    print_result("restart_service", r)

    print("\n[*] Done. Check the dashboard at http://localhost:8080\n")


if __name__ == "__main__":
    try:
        main()
    except requests.ConnectionError:
        print("Error: Cannot connect to AgentShield. Is the server running?")
        print("Start it with: ./agentshield")
        sys.exit(1)
