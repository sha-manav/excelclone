#!/usr/bin/env bash
# Start the Gridline dev environment: server + web app.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo run -p server &
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null || true' EXIT

cd apps/web && npm run dev
