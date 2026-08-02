#!/usr/bin/env bash
# Upload a local events.jsonl to the Fly volume and restart so reducer state reloads.
#
# Usage:
#   scripts/fly-events-push.sh <local-path> [--no-restart] [--yes]
#
# Steps:
#   1. Resolve machine
#   2. Backup remote /data/events.jsonl -> /data/events.jsonl.bak.<utc>
#   3. Upload local file to /data/events.jsonl.upload then atomic mv into place
#   4. Restart the machine (unless --no-restart)
#   5. Probe /healthz
#
# Env:
#   FLY_APP, FLY_MACHINE, REMOTE_EVENTS  (same as pull)
#   SKIP_CONFIRM=1                       same as --yes
#
# Example:
#   scripts/fly-events-push.sh ./tmp/events.cleaned.jsonl --yes

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=fly-events-common.sh
source "$SCRIPT_DIR/fly-events-common.sh"

LOCAL_PATH=""
NO_RESTART=0
YES=0

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-restart) NO_RESTART=1; shift ;;
    --yes|-y) YES=1; shift ;;
    -h|--help) usage ;;
    -*)
      echo "error: unknown flag: $1" >&2
      usage
      ;;
    *)
      if [[ -n "$LOCAL_PATH" ]]; then
        echo "error: unexpected argument: $1" >&2
        usage
      fi
      LOCAL_PATH="$1"
      shift
      ;;
  esac
done

if [[ -z "$LOCAL_PATH" ]]; then
  echo "error: local path required" >&2
  usage
fi
if [[ ! -f "$LOCAL_PATH" ]]; then
  echo "error: not a file: $LOCAL_PATH" >&2
  exit 1
fi
if [[ "${SKIP_CONFIRM:-0}" == "1" ]]; then
  YES=1
fi

FLY="$(fly_bin)"
MACHINE="$(resolve_machine "$FLY_APP")"
LOCAL_BYTES="$(wc -c <"$LOCAL_PATH" | tr -d ' ')"
LOCAL_LINES="$(wc -l <"$LOCAL_PATH" | tr -d ' ')"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
REMOTE_BAK="${REMOTE_EVENTS}.bak.${STAMP}"
REMOTE_UPLOAD="${REMOTE_EVENTS}.upload.${STAMP}"

echo "push: app=$FLY_APP machine=$MACHINE"
echo "push: local=$LOCAL_PATH ($LOCAL_LINES lines, $LOCAL_BYTES bytes)"
echo "push: remote=$REMOTE_EVENTS"
echo "push: backup=$REMOTE_BAK"
if [[ "$NO_RESTART" -eq 1 ]]; then
  echo "push: restart=skipped"
else
  echo "push: restart=yes (after upload)"
fi

if [[ "$YES" -ne 1 ]]; then
  read -r -p "Overwrite production event log and reload? [y/N] " ans
  case "$ans" in
    y|Y|yes|YES) ;;
    *) echo "aborted"; exit 1 ;;
  esac
fi

# fly ssh -C splits argv awkwardly; always run through sh -c.
remote_sh() {
  local cmd="$1"
  "$FLY" ssh console -a "$FLY_APP" --machine "$MACHINE" -C "sh -c $(printf '%q' "$cmd")"
}

echo "push: backing up remote log..."
remote_sh "cp -f '$REMOTE_EVENTS' '$REMOTE_BAK' && ls -la '$REMOTE_EVENTS' '$REMOTE_BAK'"
# Verify backup is non-empty and matches source size (guards against silent truncate).
remote_sh "src=\$(wc -c <'$REMOTE_EVENTS'); bak=\$(wc -c <'$REMOTE_BAK'); echo \"sizes src=\$src bak=\$bak\"; test \"\$src\" -gt 0 && test \"\$src\" -eq \"\$bak\""

echo "push: uploading to $REMOTE_UPLOAD ..."
rm -f "${LOCAL_PATH}.flyupload" 2>/dev/null || true
"$FLY" sftp put "$LOCAL_PATH" "$REMOTE_UPLOAD" -a "$FLY_APP" --machine "$MACHINE"

echo "push: atomically replacing $REMOTE_EVENTS ..."
remote_sh "mv -f '$REMOTE_UPLOAD' '$REMOTE_EVENTS' && ls -la '$REMOTE_EVENTS' '$REMOTE_BAK' && wc -l '$REMOTE_EVENTS' && test \$(wc -c <'$REMOTE_EVENTS') -eq $LOCAL_BYTES"

if [[ "$NO_RESTART" -eq 0 ]]; then
  echo "push: restarting machine $MACHINE ..."
  "$FLY" machine restart "$MACHINE" -a "$FLY_APP"
  echo "push: waiting for health..."
  ok=0
  for i in $(seq 1 36); do
    if curl -fsS --max-time 5 "https://${FLY_APP}.fly.dev/healthz" >/dev/null 2>&1; then
      ok=1
      break
    fi
    sleep 5
  done
  if [[ "$ok" -eq 1 ]]; then
    echo "push: healthz ok"
  else
    echo "push: warning — healthz did not pass within ~3m; check: fly status -a $FLY_APP" >&2
    exit 1
  fi
fi

echo "push: done"
echo "push: remote backup retained at $REMOTE_BAK"
