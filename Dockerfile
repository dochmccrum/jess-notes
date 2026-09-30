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
# The other workspace members only need to exist for Cargo to load the workspace; they aren't built.
COPY apps/native apps/native
COPY apps/tauri/src-tauri/Cargo.toml apps/tauri/src-tauri/Cargo.toml
COPY apps/tauri/src-tauri/src apps/tauri/src-tauri/src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p jess-server \
 && cargo build -p jess-core-wasm --target wasm32-unknown-unknown --profile wasm-release \
 && mkdir -p /out/wasm \
 && cp target/release/jess /out/jess \
 && wasm-bindgen --target web --out-dir /out/wasm --out-name core target/wasm32-unknown-unknown/wasm-release/jess_core_wasm.wasm \
 && (wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int -o /out/wasm/core_bg.wasm /out/wasm/core_bg.wasm || echo "wasm-opt failed; keeping unoptimised wasm")

# ---- 1b. pdfium (PDF thumbnails and text, DESIGN §8) -------------------------------------------
# The pdfium-binaries build (chromium/7999) as shipped in the pypdfium2 5.13.0 wheel on PyPI:
# immutable files with published SHA-256s, verified here.
FROM ${REGISTRY}/rust:1.93-bookworm AS pdfium
ARG TARGETARCH
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) url=https://files.pythonhosted.org/packages/d3/7c/74a2fb48e5b0d2402d9ca64b39074c722d67e9a8a2c58449a843a8c2329a/pypdfium2-5.13.0-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl; \
             sum=81df25c1ab4c13ff773102d3cbea1967511d079123b067fc077bd0c4d57d91d8 ;; \
      arm64) url=https://files.pythonhosted.org/packages/fe/31/f8210d53775f142be934336665b1d60e800c3f176f28c29b4908d945c518/pypdfium2-5.13.0-py3-none-manylinux_2_17_aarch64.manylinux2014_aarch64.whl; \
             sum=9ee8c2bb2e68b396ab4a763215ac100dacb6b96d0da5bebeb239a021aecc3a7e ;; \
      *) echo "unsupported arch ${TARGETARCH}"; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/p.whl "$url"; \
    echo "$sum  /tmp/p.whl" | sha256sum -c -; \
    unzip -q -j /tmp/p.whl 'pypdfium2_raw/libpdfium.so' -d /out

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
COPY --from=pdfium /out/libpdfium.so /usr/local/lib/libpdfium.so
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
