.PHONY: dev demo dataset agent evaluate parity server web wasm test lint fmt ci

# Start server + web dev environment
dev:
	./scripts/dev.sh

# Seed, mine and export the demonstration scenario into ./demo
demo:
	./scripts/demo.sh

# Regenerate the agent-training dataset from ./corpus/env into ./dataset
dataset:
	./scripts/dataset.sh

# Run the agent over the generated corpus and print a scorecard
agent:
	cargo run -q -p agent --bin gridline-agent -- solve \
	  --store dataset/snapshots --tasks dataset/variant-tasks.jsonl || true

# Score the agent against the corpus and decide whether memory earns promotion
evaluate:
	cargo run -q -p agent --bin gridline-agent -- evaluate \
	  --store dataset/snapshots --tasks dataset/variant-tasks.jsonl \
	  --policy rules --out runs/rules.json
	cargo run -q -p agent --bin gridline-agent -- solve \
	  --store dataset/snapshots --tasks dataset/variant-tasks.jsonl \
	  --memory runs/plans.jsonl >/dev/null || true
	cargo run -q -p agent --bin gridline-agent -- evaluate \
	  --store dataset/snapshots --tasks dataset/variant-tasks.jsonl \
	  --policy memo --memory runs/plans.jsonl --out runs/memo.json
	cargo run -q -p agent --bin gridline-agent -- promote \
	  --incumbent runs/rules.json \
	  --candidate runs/memo.json

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
