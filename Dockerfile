# Multi-stage build for apollo-mcp-server
FROM rust:1.75-slim as builder

# Set build arguments
ARG CF_REVISION
ARG CF_BRANCH

# Set environment variables
ENV CF_REVISION=${CF_REVISION}
ENV CF_BRANCH=${CF_BRANCH}

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy the entire workspace
COPY . .

# Build the apollo-mcp-server binary
RUN cargo build --release --package apollo-mcp-server

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -r -s /bin/false app

# Set working directory
WORKDIR /app

# Copy the binary from builder stage
COPY --from=builder /app/target/release/apollo-mcp-server /app/apollo-mcp-server

# Change ownership to non-root user
RUN chown app:app /app/apollo-mcp-server

# Switch to non-root user
USER app

# Expose port (adjust if your app uses a different port)
EXPOSE 8080

# Set the entrypoint
ENTRYPOINT ["/app/apollo-mcp-server"]

# Default command (can be overridden)
CMD ["--help"] 