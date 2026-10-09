#!/usr/bin/env bash
# Builds the Ubuntu .deb inside an ubuntu:26.04 container, so the binary links
# against Ubuntu's libraries rather than Nix store paths.
#
# Usage: scripts/package-deb.sh            -> dist/buzz-gpui_<version>_amd64.deb
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cache="$HOME/.cache/buzz-gpui-deb"
mkdir -p "$here/dist" "$cache/cargo" "$cache/rustup" "$cache/target"
# Never leave an older package around to be mistaken for this build.
rm -f "$here"/dist/*.deb

docker run --rm \
  -v "$here:$here" \
  -v "$cache/cargo:/root/.cargo" \
  -v "$cache/rustup:/root/.rustup" \
  -v "$cache/target:$here/target-deb" \
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
      libssl-dev libzstd-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev >/dev/null
    # Toolchains live in the mounted rustup cache; install one if it is empty.
    if [ ! -x /root/.cargo/bin/rustup ]; then
      curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --no-modify-path
    fi
    . /root/.cargo/env
    rustup toolchain list | grep -q stable || rustup default stable
    command -v cargo-deb >/dev/null || cargo install -q --locked cargo-deb
    # cargo-deb maps the target/release asset paths onto CARGO_TARGET_DIR.
    cargo build --locked --release -p buzz-gpui
    cargo deb --no-build -p buzz-gpui -o dist/
    chown -R "$OWNER" dist
  '
ls -la "$here"/dist/*.deb
