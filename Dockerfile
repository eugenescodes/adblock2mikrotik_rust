# Stage 1: Builder
FROM rust:1.95-slim AS builder

WORKDIR /build

# Install compilation dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy Cargo files
COPY Cargo.toml Cargo.lock ./

# Prebuild dependencies (speeds up repeated builds)
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy actual source code
COPY src ./src

# Compile and strip binary to reduce size
RUN cargo build --release && \
    strip target/release/adblock2mikrotik_rust

# Stage 2: Runtime (minimal image)
FROM debian:stable-slim

# Install only necessary runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user early (before COPY, no extra layer with chown /app)
RUN useradd --system --no-create-home appuser

WORKDIR /app

# Copy compiled binary
COPY --from=builder /build/target/release/adblock2mikrotik_rust /usr/local/bin/adblock2mikrotik_rust

# Dedicated output dir owned by appuser — avoids permission conflict with volume mounts
RUN mkdir /output && chown appuser:appuser /output

# Declare the output directory as an environment variable for use in the binary
ENV OUTPUT_DIR=/output

# Switch to the non-root user for better security
USER appuser

# Declare the output directory as a volume to allow users to mount it at runtime
VOLUME /output

ENTRYPOINT ["/usr/local/bin/adblock2mikrotik_rust"]