#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
clojure -M -m test.runner
clojure -M -m test.runner browser-sse
clojure -M -m test.runner browser-post-redact
