#!/usr/bin/env bash
# Download production events.jsonl from the Fly volume to a local path.
#
# Usage:
#   scripts/fly-events-pull.sh [local-path]
#
# Env:
#   FLY_APP          app name (default: slugsocial)
#   FLY_MACHINE      pin a machine id (optional; otherwise first started machine)
#   REMOTE_EVENTS    remote path (default: /data/events.jsonl)
#
# Example:
#   scripts/fly-events-pull.sh
#   scripts/fly-events-pull.sh ./tmp/events.jsonl

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=fly-events-common.sh
source "$SCRIPT_DIR/fly-events-common.sh"

LOCAL_PATH="${1:-./tmp/events.jsonl}"
FLY="$(fly_bin)"
MACHINE="$(resolve_machine "$FLY_APP")"

mkdir -p "$(dirname "$LOCAL_PATH")"

echo "pull: app=$FLY_APP machine=$MACHINE"
echo "pull: $REMOTE_EVENTS -> $LOCAL_PATH"

# fly sftp get refuses to overwrite an existing local file — use a fresh path.
tmp="${LOCAL_PATH}.partial.$$"
rm -f "$tmp"
cleanup() { rm -f "$tmp"; }
trap cleanup EXIT

"$FLY" sftp get "$REMOTE_EVENTS" "$tmp" -a "$FLY_APP" --machine "$MACHINE"
mv -f "$tmp" "$LOCAL_PATH"
trap - EXIT

bytes="$(wc -c <"$LOCAL_PATH" | tr -d ' ')"
lines="$(wc -l <"$LOCAL_PATH" | tr -d ' ')"
echo "pull: ok ($lines lines, $bytes bytes) -> $LOCAL_PATH"
