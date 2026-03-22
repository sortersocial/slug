#!/usr/bin/env bash
# TEST.sh — onboarding entry point for slug.social
#
# Clones the repo, runs this, everything is verified.
# Runs the full round-trip sanity check: build → ingest → query → kill → replay → verify.

set -euo pipefail

echo "=== slug.social test suite ==="
echo "Running: bb sanity"
echo ""

bb sanity
