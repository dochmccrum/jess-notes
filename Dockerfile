# Jess Notes server + web UI. One image, one volume (/data). See docs/DEPLOY.md.
#
#   docker build -t jess .
#   docker run -p 8080:8080 -v jess-data:/data jess
#
# Base images come from Docker Hub; pass --build-arg REGISTRY=mirror.gcr.io/library (or another
# mirror) if Docker Hub rate-limits you.
ARG REGISTRY=docker.io/library

# ---- 1. Rust: server binary and core.wasm --------------------------------------------------
FROM ${REGISTRY}/rust:1.93-bookworm AS rust
ARG WASM_BINDGEN_VERSION=0.2.129
RUN rustup target add wasm32-unknown-unknown \
 && apt-get update && apt-get install -y --no-install-recommends binaryen && rm -rf /var/lib/apt/lists/* \
 && cargo install --locked wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION}
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core core
COPY server server
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p jess-server \
 && cargo build -p jess-core-wasm --target wasm32-unknown-unknown --profile wasm-release \
 && mkdir -p /out/wasm \
 && cp target/release/jess /out/jess \
 && wasm-bindgen --target web --out-dir /out/wasm --out-name core target/wasm32-unknown-unknown/wasm-release/jess_core_wasm.wasm \
 && (wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int -o /out/wasm/core_bg.wasm /out/wasm/core_bg.wasm || echo "wasm-opt failed; keeping unoptimised wasm")

# ---- 2. Web UI -------------------------------------------------------------------------------
FROM ${REGISTRY}/node:22-bookworm-slim AS ui
RUN corepack enable
WORKDIR /src/ui
COPY ui/package.json ui/pnpm-lock.yaml ui/pnpm-workspace.yaml ./
RUN --mount=type=cache,target=/root/.local/share/pnpm/store pnpm install --frozen-lockfile
COPY ui ./
COPY bench ../bench
COPY --from=rust /out/wasm ./src/wasm
RUN pnpm build

# ---- 3. Runtime ------------------------------------------------------------------------------
FROM ${REGISTRY}/debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git openssh-client tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 1000 --create-home --home-dir /home/jess jess \
 && mkdir -p /data && chown jess:jess /data
COPY --from=rust /out/jess /usr/local/bin/jess
COPY --from=ui /src/ui/dist /app/ui
ENV JESS_DATA_DIR=/data \
    JESS_UI_DIR=/app/ui \
    PORT=8080 \
    RUST_LOG=info
USER jess
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 CMD ["jess", "health"]
ENTRYPOINT ["/usr/bin/tini", "--", "jess"]
CMD ["serve"]
