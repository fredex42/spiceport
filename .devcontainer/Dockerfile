FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        ninja-build \
        python3 \
        python3-venv \
    && rm -rf /var/lib/apt/lists/*

COPY --from=ghcr.io/jdx/mise:latest /usr/local/bin/mise /usr/local/bin/mise

ENV PATH="/home/vscode/.local/share/mise/shims:${PATH}"