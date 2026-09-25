#!/bin/bash
# Mock plugin that registers hooks and subscriptions for testing.
# Registers: openchannel hook, channel_ready subscription, "greet" RPC method.
# The openchannel hook rejects channels with funding < 50000 sats.

while IFS= read -r line; do
    method=$(echo "$line" | sed -n 's/.*"method"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    id=$(echo "$line" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p')

    case "$method" in
        "getmanifest")
            echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"result\":{\"rpc_methods\":[{\"name\":\"greet\",\"description\":\"Greets\",\"usage\":\"\"}],\"hooks\":[{\"name\":\"openchannel\",\"before\":[],\"after\":[]}],\"subscriptions\":[\"channel_ready\"],\"options\":[],\"dynamic\":true,\"failure_mode\":\"fail_open\"}}"
            ;;
        "init")
            echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"result\":{}}"
            ;;
        "greet")
            echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"result\":{\"greeting\":\"hi there!\"}}"
            ;;
        "hook/openchannel")
            # Reject channels under 50000 sats
            funding=$(echo "$line" | sed -n 's/.*"funding_satoshis"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p')
            if [ -n "$funding" ] && [ "$funding" -lt 50000 ] 2>/dev/null; then
                echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"result\":{\"result\":\"reject\",\"message\":\"channel too small\"}}"
            else
                echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"result\":{\"result\":\"continue\"}}"
            fi
            ;;
        "channel_ready")
            # Notification — no response needed (no id)
            ;;
        "shutdown")
            exit 0
            ;;
        *)
            echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32601,\"message\":\"method not found\"}}"
            ;;
    esac
done
