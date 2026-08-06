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

# The engine has to be compiled to wasm before the web app can resolve it —
# and it has to be rebuilt when the engine changes, not just when the package
# is missing.
#
# This used to test only for the file's existence, which is fine on a fresh
# clone and quietly wrong after a `git pull`: Vite serves the new TypeScript
# against the old `pkg`, the app calls a wasm export that build does not have,
# the module throws during init, and React never mounts. The page comes up
# blank with the title set, which looks like a broken app rather than a stale
# artifact.
#
# The marker is `package.json` rather than the `.wasm` blob, because wasm-pack
# writes it last. A build that dies partway — the wasm-opt download failing
# behind a proxy is the one we hit — leaves the blob behind without a manifest,
# and `gridline-wasm` is a `file:` dependency, so npm's symlink then points at
# a directory Vite cannot resolve as a package. Keying off the blob would call
# that wreckage fresh and serve it forever, since nothing in the sources is
# newer than it.
WASM_STAMP=crates/wasm/pkg/package.json
wasm_is_stale() {
  [ -f "$WASM_STAMP" ] || return 0
  # Any engine or binding source newer than what was built from it?
  [ -n "$(find crates/engine/src crates/wasm/src \
                crates/engine/Cargo.toml crates/wasm/Cargo.toml \
                -newer "$WASM_STAMP" -print 2>/dev/null | head -1)" ]
}
if wasm_is_stale; then
  if [ -f "$WASM_STAMP" ]; then
    echo "==> the engine changed since the browser build; rebuilding"
  else
    echo "==> building the engine for the browser (first run)"
  fi
  # `--no-opt` for the dev server: wasm-opt only shrinks the artifact, which
  # matters for what users download and not at all for what a developer runs.
  # It also costs a download of the binaryen toolchain on first use, so
  # skipping it makes the dev loop faster and removes a network dependency
  # from it. `make wasm` and CI still build the optimized package.
  wasm-pack build crates/wasm --target web --no-opt --out-dir pkg
fi

if [ ! -d apps/web/node_modules ]; then
  echo "==> installing web dependencies (first run)"
  (cd apps/web && npm install --no-audit --no-fund)
fi

echo "==> building the server"
cargo build -p server

# A token file that exists but is *empty* is worse than one that is missing:
# the web app sends `Authorization: Bearer ` with every request, the server
# answers 401, and a 401 is not retryable — so the capture queue discards the
# batch and the app goes on reporting "capturing, 0 waiting" while every event
# is thrown away. That is exactly what happened: an earlier run died at this
# step (the two-binary ambiguity, fixed since), and `> .dev-token` had already
# created the file, so the `-f` guard never let it be seeded again.
#
# So: test for a non-empty token, and write through a temporary file so a
# failed seed leaves nothing behind rather than a booby trap.
if [ ! -s .dev-token ]; then
  echo "==> seeding a development user"
  # The token is printed once; keep it where the web app can read it.
  cargo run -q -p server -- --seed-user --admin | tee /dev/stderr \
    | awk '/^token:/ { print $2 }' > .dev-token.tmp
  if [ -s .dev-token.tmp ]; then
    mv .dev-token.tmp .dev-token
  else
    rm -f .dev-token.tmp
    echo "error: seeding produced no token; capture would silently fail" >&2
    exit 1
  fi
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
