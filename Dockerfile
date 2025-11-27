# Build stage
FROM rust:1-slim-bookworm AS builder

WORKDIR /build

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src
COPY benches ./benches

# Build release binary
RUN cargo build --release --locked

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /build/target/release/terragrunt-dag /usr/local/bin/terragrunt-dag

# Create non-root user
RUN useradd -m -u 1000 terragrunt && \
    chown -R terragrunt:terragrunt /usr/local/bin/terragrunt-dag

USER terragrunt

WORKDIR /workspace

ENTRYPOINT ["terragrunt-dag"]
CMD ["--help"]
