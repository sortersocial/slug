#!/usr/bin/env bash
set -euo pipefail
cargo test --all
./scripts/clj-test.sh
