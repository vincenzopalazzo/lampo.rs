#!/usr/bin/env python3
"""Policy Demo — demonstrates live policy change (the key investor moment).

This script shows how changing policy in real-time alters agent behavior
without any changes to agent code.

Scenario:
  1. Send $20 payment → auto-approved (limit is $100)
  2. Change policy limit to $10
  3. Send $20 payment again → now requires approval
"""

import sys
import time
import requests

BASE_URL = "http://localhost:8080/api/v1"
AGENT_ID = "finance-agent-01"


def submit_payment(amount: float) -> dict:
    resp = requests.post(f"{BASE_URL}/actions", json={
        "agent_id": AGENT_ID,
        "action_type": "send_payment",
        "parameters": {"amount": amount, "currency": "USD", "recipient": "demo@example.com"},
    })
    resp.raise_for_status()
    return resp.json()


def get_policy() -> dict:
    resp = requests.get(f"{BASE_URL}/policy")
    resp.raise_for_status()
    return resp.json()


def update_policy(limit: float) -> dict:
    resp = requests.patch(f"{BASE_URL}/policy", json={
        "auto_approve_payment_limit": limit,
    })
    resp.raise_for_status()
    return resp.json()


def pr(label, result):
    s = result["status"]
    colors = {"executed": "\033[92m", "pending": "\033[93m", "denied": "\033[91m"}
    reset = "\033[0m"
    c = colors.get(s, "")
    print(f"  {label}: {c}{s}{reset} (decision: {result['decision']}, reason: {result['reason']})")


def main():
    print("\n" + "="*60)
    print("  AgentShield — Live Policy Control Demo")
    print("  This is the key investor moment.")
    print("="*60)

    # Step 1: Check current policy
    p = get_policy()
    print(f"\n[1] Current auto-approve limit: ${p['auto_approve_payment_limit']:.0f}")

    # Step 2: Send $20 — should be auto-approved
    print("\n[2] Sending $20 payment...")
    r = submit_payment(20)
    pr("$20 payment", r)

    # Step 3: Change policy
    print("\n[3] Changing auto-approve limit from $100 → $10...")
    time.sleep(1)  # dramatic pause for demo
    p = update_policy(10)
    print(f"    New limit: ${p['auto_approve_payment_limit']:.0f}")

    # Step 4: Send same $20 — now requires approval
    print("\n[4] Sending $20 payment again (same agent, same code)...")
    r = submit_payment(20)
    pr("$20 payment", r)

    print(f"\n{'='*60}")
    print("  The same $20 payment now requires approval!")
    print("  No agent code was changed — only the policy.")
    print("  This is what makes AgentShield a control plane.")
    print(f"{'='*60}\n")

    print("  Open http://localhost:8080 to approve the pending action.\n")


if __name__ == "__main__":
    try:
        main()
    except requests.ConnectionError:
        print("Error: Cannot connect to AgentShield. Is the server running?")
        sys.exit(1)
