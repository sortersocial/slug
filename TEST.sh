#!/usr/bin/env bash
# TEST.sh — entry point for verifying a fresh clone of slug.social
#
# Runs the full round-trip sanity check: build → ingest → query → kill → replay → verify.
# Exit code reflects the result of the sanity check.

set -euo pipefail

echo "slug.social — running sanity check"
echo "===================================="

bb sanity
