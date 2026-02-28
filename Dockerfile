# Build stage
FROM rust:1.93.1-bookworm@sha256:7c4ae649a84014c467d79319bbf17ce2632ae8b8be123ac2fb2ea5be46823f31 AS builder

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

# Build the release binary
RUN cargo build --release --package apollo-mcp-server --bin apollo-mcp-server

# Runtime stage - minimal image with just glibc
# Using distroless/cc which includes glibc and CA certificates
FROM gcr.io/distroless/cc-debian12

# MCP Registry annotation for publishing
LABEL io.modelcontextprotocol.server.name="io.github.apollographql/apollo-mcp-server"

# Copy the binary
COPY --from=builder /app/target/release/apollo-mcp-server /usr/local/bin/apollo-mcp-server

# Create /data directory
# WORKDIR creates the directory if it doesn't exist
WORKDIR /data

# Environment variables
ENV APOLLO_MCP_TRANSPORT__TYPE=streamable_http
ENV APOLLO_MCP_TRANSPORT__ADDRESS=0.0.0.0

# Expose port
EXPOSE 8000/tcp

# Run as non-root user
USER 1000:1000

# Entrypoint and Cmd
ENTRYPOINT ["apollo-mcp-server"]
CMD ["/dev/null"]
