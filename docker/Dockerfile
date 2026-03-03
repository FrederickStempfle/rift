FROM rust:1.82-bookworm AS builder
WORKDIR /app

# clang is needed for linking V8 (deno_core dependency)
RUN apt-get update && apt-get install -y --no-install-recommends clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
ENV CC=clang CXX=clang++

COPY Cargo.toml rust-toolchain.toml ./
COPY apps/engine/Cargo.toml apps/engine/Cargo.toml
COPY apps/engine/src apps/engine/src
COPY apps/engine/migrations apps/engine/migrations

RUN cargo build --release -p rift-engine

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl bash postgresql-client iptables unzip git python3 make g++ gnupg \
    && rm -rf /var/lib/apt/lists/*

# Install Docker CLI (for managing Supabase via Docker Compose)
RUN curl -fsSL https://download.docker.com/linux/debian/gpg | gpg --dearmor -o /usr/share/keyrings/docker-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/docker-archive-keyring.gpg] https://download.docker.com/linux/debian bookworm stable" > /etc/apt/sources.list.d/docker.list \
    && apt-get update && apt-get install -y --no-install-recommends docker-ce-cli docker-compose-plugin \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deno.land/install.sh | DENO_INSTALL=/usr/local sh \
    && ln -s /usr/local/bin/deno /usr/bin/deno

RUN curl -fsSL https://deb.nodesource.com/setup_24.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g yarn pnpm serve

RUN curl -fsSL https://bun.sh/install | BUN_INSTALL=/usr/local bash

RUN useradd --system --uid 1000 --create-home --home-dir /var/lib/rift rift

WORKDIR /app

COPY --from=builder /app/target/release/rift-engine /usr/local/bin/rift-engine
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh

# Copy templates for worker pool mode (worker loader, wrappers, function router)
COPY templates/ /opt/rift/templates/

RUN chmod +x /usr/local/bin/entrypoint.sh \
    && mkdir -p /var/rift/deployments /var/rift/builds /var/rift/ssl /opt/rift/templates /opt/rift/services

ENV RUST_LOG=rift_engine=info,tower_http=info
EXPOSE 3001 8080

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
