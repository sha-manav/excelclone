#!/usr/bin/env bash
# Start the Gridline dev environment: API server + web app.
#
# On first run this seeds a user and prints its token. Paste that token into
# the browser console as shown, or it will be picked up from .dev-token by
# the Vite dev server.
set -euo pipefail
cd "$(dirname "$0")/.."

export PATH="/opt/homebrew/opt/rustup/bin:$PATH:$HOME/.cargo/bin"
export DATABASE_URL="${DATABASE_URL:-sqlite://$(pwd)/gridline.db?mode=rwc}"
export PORT="${PORT:-8787}"

# The engine has to be compiled to wasm before the web app can resolve it.
if [ ! -f crates/wasm/pkg/gridline_wasm_bg.wasm ]; then
  echo "==> building the engine for the browser (first run)"
  wasm-pack build crates/wasm --target web --out-dir pkg
fi

if [ ! -d apps/web/node_modules ]; then
  echo "==> installing web dependencies (first run)"
  (cd apps/web && npm install --no-audit --no-fund)
fi

echo "==> building the server"
cargo build -p server

if [ ! -f .dev-token ]; then
  echo "==> seeding a development user"
  # The token is printed once; keep it where the web app can read it.
  cargo run -q -p server -- --seed-user --admin | tee /dev/stderr \
    | awk '/^token:/ { print $2 }' > .dev-token
fi
TOKEN="$(cat .dev-token)"

cargo run -q -p server &
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null || true' EXIT

# Wait for the API before starting the UI, so the first request cannot race.
until curl -fsS "http://localhost:$PORT/health" >/dev/null 2>&1; do sleep 0.3; done
echo "==> API listening on http://localhost:$PORT"

cat <<EOF

  Development token (already in .dev-token):
    $TOKEN

  The web app adopts it from VITE_DEV_TOKEN on first load (dev builds only),
  so no manual step is needed. A token already in localStorage wins.

EOF

cd apps/web && VITE_DEV_TOKEN="$TOKEN" VITE_API_URL="http://localhost:$PORT" npm run dev
