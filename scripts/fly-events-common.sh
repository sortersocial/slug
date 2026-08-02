#!/usr/bin/env bash
# Shared helpers for fly-events-{pull,push}.sh
# Sourced only — do not execute directly.

FLY_APP="${FLY_APP:-slugsocial}"
REMOTE_EVENTS="${REMOTE_EVENTS:-/data/events.jsonl}"

fly_bin() {
  if command -v flyctl >/dev/null 2>&1; then
    echo flyctl
  elif command -v fly >/dev/null 2>&1; then
    echo fly
  else
    echo "error: neither flyctl nor fly found on PATH" >&2
    return 1
  fi
}

# Resolve a started machine id. Honors FLY_MACHINE if set.
resolve_machine() {
  local fly app mid
  fly="$(fly_bin)" || return 1
  app="${1:-$FLY_APP}"

  if [[ -n "${FLY_MACHINE:-}" ]]; then
    echo "$FLY_MACHINE"
    return 0
  fi

  mid="$("$fly" machines list -a "$app" --json 2>/dev/null \
    | python3 -c '
import json, sys
machines = json.load(sys.stdin)
started = [m for m in machines if (m.get("state") or "").lower() == "started"]
pick = started or machines
if not pick:
    sys.exit("no machines found")
print(pick[0]["id"])
')" || return 1

  if [[ -z "$mid" ]]; then
    echo "error: could not resolve machine id for app=$app" >&2
    return 1
  fi
  echo "$mid"
}
