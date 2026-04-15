#!/usr/bin/env bash
# Deploy / update the Open WebUI Fly app (see deploy/open-webui/README.md for one-time setup:
# app create, volume, secrets).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
exec fly deploy --config "$ROOT/deploy/open-webui/fly.toml" "$@"
