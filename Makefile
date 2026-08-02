.PHONY: dev demo server web wasm test lint fmt ci

# Start server + web dev environment
dev:
	./scripts/dev.sh

# Seed, mine and export the demonstration scenario into ./demo
demo:
	./scripts/demo.sh

server:
	cargo run -p server

web:
	cd apps/web && npm run dev

wasm:
	wasm-pack build crates/wasm --target web --out-dir pkg

test:
	cargo test --workspace

fmt:
	cargo fmt --all

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

ci: lint test
	cd apps/web && npm run build
