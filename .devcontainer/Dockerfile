FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        g++ \
        git \
        pkg-config \
        # Headers SDL3 needs when built from source (sdl3 build-from-source-static);
        # without them SDL only gets its dummy/offscreen video and audio drivers.
        libasound2-dev \
        libdbus-1-dev \
        libegl-dev \
        libgl-dev \
        libpulse-dev \
        libudev-dev \
        libwayland-dev \
        libx11-dev \
        libxcursor-dev \
        libxext-dev \
        libxfixes-dev \
        libxi-dev \
        libxkbcommon-dev \
        libxrandr-dev \
        libxss-dev \
        libxtst-dev \
        wayland-protocols \
        ninja-build \
        python3 \
        python3-venv \
    && rm -rf /var/lib/apt/lists/*

COPY --from=ghcr.io/jdx/mise:latest /usr/local/bin/mise /usr/local/bin/mise

ENV PATH="/home/vscode/.local/share/mise/shims:${PATH}"