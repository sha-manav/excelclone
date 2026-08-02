# Decisions

One-line rationale for every non-obvious choice.

- **Repo root is `/Users/manavshah/excelclone` (existing empty dir), not a nested `gridline/`** — the working directory was created for this project; nesting would add noise.
- **Rust installed via Homebrew `rustup` (keg-only) + stable toolchain** — no toolchain existed on the machine; rustup is required for wasm32 target management.
- **Version pinning via committed `Cargo.lock` + `package-lock.json` rather than `=x.y.z` in manifests** — lockfiles pin exact versions for every build (incl. transitive deps) while keeping manifests readable; CI uses the lockfiles.
- **Wasm crate named `gridline-wasm` (dir `crates/wasm`)** — crate name `wasm` would collide conceptually with the ecosystem; dir layout follows the spec.
- **`serde-wasm-bindgen` for JS↔Rust value passing** — boring, maintained, avoids JSON string round-trips where possible.
- **CI on `dtolnay/rust-toolchain` + `Swatinem/rust-cache`** — the standard boring GitHub Actions setup for Rust workspaces.
- **getrandom wasm_js backend cfg in `.cargo/config.toml` + target-specific getrandom dep in gridline-wasm** — ulid→rand→getrandom 0.3 requires an explicit JS entropy backend on wasm32-unknown-unknown.
