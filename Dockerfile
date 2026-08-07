# syntax=docker/dockerfile:1

# Builder stage: compiles the release binary inside the image, so `docker
# build .` works standalone with no separate CI pre-build step.
FROM rust:1-slim-bookworm@sha256:96c0af8cf054fd006435089f0076729716784ec9be485bd655de59c55df105ce AS builder

# native-tls links against system OpenSSL at build time.
RUN apt-get update && \
    apt-get install -y --no-install-recommends pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation separately from source changes: this layer
# only invalidates when Cargo.toml/Cargo.lock change, not on every edit.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release --locked && \
    rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

# Runtime stage (Debian Bookworm slim - provides glibc 2.36+ and openssl 3)
FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

# Install only runtime dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 && \
    rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*

# Create non-root user with no shell
RUN groupadd -g 1000 exporter && \
    useradd -m -u 1000 -g exporter -s /sbin/nologin exporter

WORKDIR /app

COPY --from=builder /build/target/release/signal /app/exporter

# Verify binary
RUN chmod +x /app/exporter

# Switch to non-root user
USER exporter

ENTRYPOINT ["/app/exporter"]
