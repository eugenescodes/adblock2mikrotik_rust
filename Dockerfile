# Stage 1: Builder
FROM rust:1.90-slim AS builder

WORKDIR /build

# Install compilation dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy Cargo files
COPY Cargo.toml Cargo.lock ./

# Create dummy main.rs for prebuild dependencies (speeds up repeated builds)
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy actual code
COPY src ./src

# Compile with optimization and strip to reduce size
RUN cargo build --release && \
    strip target/release/adblock2mikrotik_rust

# Stage 2: Runtime (minimal image)
FROM debian:stable-slim

WORKDIR /app

# Install only necessary runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    curl \
    wget \
    && rm -rf /var/lib/apt/lists/*

# Copy compiled binary
COPY --from=builder /build/target/release/adblock2mikrotik_rust /app/adblock2mikrotik_rust

ENTRYPOINT ["/app/adblock2mikrotik_rust"]
CMD []
