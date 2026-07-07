#!/usr/bin/env bash

DIR="$1"

SCRIPT_FILE="tests/$DIR/script.js"
CONTEXT_FILE="tests/$DIR/context.json"
CONFIG_FILE="tests/$DIR/secrets.json"

# Include the per-test config block only if the test dir provides one.
# Capabilities are opt-in: the global is injected only when its config is
# present, so a missing secrets.json => `mail`/`db` is undefined. Driver-backed
# capabilities (db/mongo/mail/redis/amq/auth) take LOGICAL NAMES via `io`
# (e.g. {"io":{"db":["local-db"]}}) — the credentials live in the fabricd
# sidecar's resources config (see fabricd.example.json in the fabricd repo,
# github.com/hlop3z/fabricd), never in the
# request. Only `api` (allowed_hosts), `s3`, and `sys` keep request-side
# config, which is why this file can still hold secrets and stays gitignored.
if [ -f "$CONFIG_FILE" ]; then
  CONFIG_JSON="$(cat "$CONFIG_FILE")"
else
  CONFIG_JSON='{}'
fi

curl -X POST http://localhost:4172/execute \
  -H "Content-Type: application/json" \
  -d "$(jq -n \
    --arg script "$(cat "$SCRIPT_FILE")" \
    --argjson context "$(cat "$CONTEXT_FILE")" \
    --argjson config "$CONFIG_JSON" \
    '{script: $script, context: $context, config: $config}'
  )"
