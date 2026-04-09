#!/usr/bin/env bash
# Run the same integration suite as `bb test` (see bb.edn :task test), on the JVM.
# Requires: Java + Clojure CLI (`clojure` or `clj` on PATH).
set -euo pipefail
cd "$(dirname "$0")/.."
exec clojure -M -m test.runner "$@"
