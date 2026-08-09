# Pinned substrate for authoritative tutorial runs (`tatu run`).
# Rust must satisfy max(tauri MSRV — 1.90 for tauri 2.x — and tatu's own
# rust-version). Bump deliberately; the image tag is part of the cache key.
FROM rust:1.93.0-bookworm

# Tauri v2 Linux build prerequisites (the tauri crate compiles gtk/webkit sys
# crates even for headless MockRuntime tests)
RUN apt-get update && apt-get install -y --no-install-recommends \
    libwebkit2gtk-4.1-dev \
    build-essential \
    curl \
    wget \
    file \
    libxdo-dev \
    libssl-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    && rm -rf /var/lib/apt/lists/*

# Node + pnpm for frontend-build assertions (bundler tutorials); node 24
# matches GitHub-hosted runners
RUN curl -fsSL https://deb.nodesource.com/setup_24.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g pnpm@10 \
    && rm -rf /var/lib/apt/lists/*

# Pre-fetch every dependency graph a run compiles — tatu's own and each
# vendored base's — so a cold cargo-home cache pays no crates.io downloads.
# Fetch only: compiled artifacts would balloon the image for a saving the
# actions cache already provides when warm. The image tag hashes these
# inputs too (.github/actions/runner-image), so the baked registry tracks
# the lockfiles.
COPY Cargo.toml Cargo.lock /warm/runner/
COPY bases /warm/bases
RUN mkdir -p /warm/runner/src && echo 'fn main() {}' > /warm/runner/src/main.rs \
    && cargo fetch --locked --manifest-path /warm/runner/Cargo.toml \
    && for base in /warm/bases/*/src-tauri; do \
         cargo fetch --locked --manifest-path "$base/Cargo.toml"; \
       done \
    && rm -rf /warm

# marks runs inside this image as authoritative
ENV TATU_CONTAINER=1

WORKDIR /work
