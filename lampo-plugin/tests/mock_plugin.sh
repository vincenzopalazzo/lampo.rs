#!/bin/bash
# Mock plugin for testing the lampo plugin system.
# Reads JSON-RPC from stdin, writes responses to stdout.
# This is a minimal example of a plugin in any language.

while IFS= read -r line; do
    # Parse method from JSON (simple extraction)
    method=$(echo "$line" | sed -n 's/.*"method"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    id=$(echo "$line" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p')

    case "$method" in
        "getmanifest")
            echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"result\":{\"rpc_methods\":[{\"name\":\"hello\",\"description\":\"Says hello\",\"usage\":\"[name]\"}],\"hooks\":[],\"subscriptions\":[],\"options\":[],\"dynamic\":true,\"failure_mode\":\"fail_open\"}}"
            ;;
        "init")
            echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"result\":{}}"
            ;;
        "hello")
            echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"result\":{\"message\":\"hello from plugin!\"}}"
            ;;
        "shutdown")
            exit 0
            ;;
        *)
            echo "{\"id\":$id,\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32601,\"message\":\"method not found\"}}"
            ;;
    esac
done
