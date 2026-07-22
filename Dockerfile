# ─── Build stage ─────────────────────────────────────────────────────────────
# Debian 13 (trixie) base: the ONNX Runtime prebuilt that `ort`/`fastembed` link
# during `cargo build` needs glibc 2.38+ / newer libstdc++, which the old bookworm
# (glibc 2.36) base cannot provide (undefined `__isoc23_strtoull` / `__cxa_call_terminate`).
FROM rust:1.92.0-trixie AS builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    perl \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy source files
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/

# Build the release binary. `ort`'s default `download-binaries` feature fetches +
# links the ONNX Runtime engine here, so the build stage needs network egress.
RUN cargo build --release --package apollo-mcp-server --bin apollo-mcp-server

# ─── Model stage ─────────────────────────────────────────────────────────────
# Bake the embedding model into the image so the running pod never needs
# HuggingFace egress (otherwise the first search silently degrades to lexical-only
# on locked-down/on-prem deployments). fastembed reads `.fastembed_cache` in the HF
# hub layout relative to its CWD (/data). `HF_HUB_CACHE` must point at that dir so
# the `models--Xenova--bge-small-en-v1.5` tree lands where fastembed later reads it.
FROM python:3.12-slim AS model
RUN pip install --no-cache-dir -q huggingface_hub
# Fetch only the files fastembed actually reads (one ONNX variant + tokenizer/config),
# not the whole repo's many ONNX variants — keeps the baked model ~128 MB.
RUN python -c "from huggingface_hub import snapshot_download; snapshot_download('Xenova/bge-small-en-v1.5', cache_dir='/model/.fastembed_cache', allow_patterns=['config.json','tokenizer.json','tokenizer_config.json','special_tokens_map.json','onnx/model.onnx'])"

# ─── Runtime stage ───────────────────────────────────────────────────────────
# Debian 13 slim + ORT's runtime deps. (distroless/cc-debian12 lacks the newer
# libstdc++ ORT needs; if gcr.io/distroless/cc-debian13 is published, prefer it and
# drop this apt install.)
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    libstdc++6 \
    libgcc-s1 \
    && rm -rf /var/lib/apt/lists/*

# MCP Registry annotation for publishing
LABEL io.modelcontextprotocol.server.name="io.github.apollographql/apollo-mcp-server"

# Copy the binary
COPY --from=builder /app/target/release/apollo-mcp-server /usr/local/bin/apollo-mcp-server

# Create /data directory (WORKDIR creates it if absent)
WORKDIR /data

# Baked embedding model, owned by the non-root runtime user (uid 1000).
COPY --from=model --chown=1000:1000 /model/.fastembed_cache /data/.fastembed_cache

# Environment variables
ENV APOLLO_MCP_TRANSPORT__TYPE=streamable_http
ENV APOLLO_MCP_TRANSPORT__ADDRESS=0.0.0.0
# Read the baked model and never attempt a network fetch for it.
ENV HF_HUB_OFFLINE=1
# uid 1000 needs a writable $HOME: a HuggingFace-lib lock file is written at
# startup (even offline), and that write fails without a writable home.
ENV HOME=/tmp

# Expose port
EXPOSE 8000/tcp

# Run as non-root user
USER 1000:1000

# Entrypoint and Cmd
ENTRYPOINT ["apollo-mcp-server"]
CMD ["/dev/null"]
