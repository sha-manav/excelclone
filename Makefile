.PHONY: dev demo dataset parity server web wasm test lint fmt ci

# Start server + web dev environment
dev:
	./scripts/dev.sh

# Seed, mine and export the demonstration scenario into ./demo
demo:
	./scripts/demo.sh

# Regenerate the agent-training dataset from ./corpus/env into ./dataset
dataset:
	./scripts/dataset.sh

# Measure Excel parity and regenerate PARITY.md
parity:
	cargo run -q -p parity --bin gridline-parity -- report

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
	cargo run -q -p parity --bin gridline-parity -- report --check
	./scripts/dataset.sh --check
	cd apps/web && npm run build
