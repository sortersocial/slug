#!/usr/bin/env bash
# TEST.sh — entry point for verifying a fresh clone of slug.social
#
# Runs the full round-trip sanity check: build → ingest → query → kill → replay → verify.
# Exit code reflects the result of the sanity check.

set -euo pipefail

echo "slug.social — running full test suite"
echo "======================================="

echo ""
echo "1/3  cargo test (unit + integration)"
cargo test --all

echo ""
echo "2/3  bb sanity (end-to-end: build → ingest → query → replay)"
bb sanity

echo ""
echo "3/3  done"
