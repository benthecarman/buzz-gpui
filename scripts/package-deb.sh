#!/usr/bin/env bash
# Builds the Ubuntu .deb inside an ubuntu:26.04 container, so the binary links
# against Ubuntu's libraries rather than Nix store paths.
#
# Usage: scripts/package-deb.sh            -> dist/buzz-gpui_<version>_amd64.deb
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$here/dist" "$HOME/.cache/buzz-gpui-deb/cargo" "$HOME/.cache/buzz-gpui-deb/target"

docker run --rm \
  -v "$here:$here" \
  -v "$HOME/.cache/buzz-gpui-deb/cargo:/root/.cargo" \
  -v "$HOME/.cache/buzz-gpui-deb/target:$here/target-deb" \
  -e CARGO_TARGET_DIR="$here/target-deb" \
  -e OWNER="$(id -u):$(id -g)" \
  -w "$here" \
  ubuntu:26.04 bash -euo pipefail -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends \
      ca-certificates curl build-essential pkg-config cmake clang \
      libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev libx11-dev \
      libx11-xcb-dev libvulkan-dev libfontconfig-dev libfreetype-dev libdbus-1-dev \
      libssl-dev libzstd-dev >/dev/null
    if [ ! -x /root/.cargo/bin/cargo ]; then
      curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal
    fi
    . /root/.cargo/env
    command -v cargo-deb >/dev/null || cargo install -q --locked cargo-deb
    # cargo-deb maps the target/release asset paths onto CARGO_TARGET_DIR.
    cargo build --locked --release -p buzz-gpui
    cargo deb --no-build -p buzz-gpui -o dist/
    chown -R "$OWNER" dist
  '
ls -la "$here"/dist/*.deb
