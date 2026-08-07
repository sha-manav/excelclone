# The Gridline API server, and nothing else.
#
# The web app is a static bundle published separately (`.github/workflows/
# pages.yml`), so this image carries no Node, no wasm toolchain and no
# front-end build — only the axum binary and the SQLite database it opens.

# ---------------------------------------------------------------- build stage
FROM rust:1-slim-bookworm AS build
WORKDIR /src

# `sqlx` with the `sqlite` feature builds a bundled SQLite from source, which
# needs a C compiler; nothing here links against a system libsqlite3.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# The whole workspace, because `server` depends on `engine` by path and Cargo
# resolves the lockfile across every member. Copying only two crates would
# produce a different dependency graph from the one CI tests.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# `--locked` so a deploy can never silently resolve a different dependency
# version from the one the test suite ran against.
RUN cargo build --release --locked -p server --bin server

# ------------------------------------------------------------ runtime stage
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --uid 10001 gridline

COPY --from=build /src/target/release/server /usr/local/bin/gridline-server

# The volume is mounted here. `mode=rwc` creates the database on first boot,
# and the migrations in the binary bring it up to date on every start.
ENV DATABASE_URL=sqlite:///data/gridline.db?mode=rwc
ENV PORT=8787
ENV RUST_LOG=server=info,tower_http=info

# Not root: the process only ever needs to write one directory.
RUN mkdir -p /data && chown gridline:gridline /data
USER gridline
VOLUME ["/data"]
EXPOSE 8787

CMD ["gridline-server"]
