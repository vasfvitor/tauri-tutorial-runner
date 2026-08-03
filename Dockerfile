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

# marks runs inside this image as authoritative
ENV TATU_CONTAINER=1

WORKDIR /work
