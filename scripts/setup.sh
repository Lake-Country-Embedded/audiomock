#!/usr/bin/env bash
set -euo pipefail

echo "=== audiomock: Development Environment Setup ==="

# System packages
echo ""
echo "--- Installing system packages ---"
sudo apt install -y \
  libpipewire-0.3-dev \
  libspa-0.2-dev \
  pipewire \
  pipewire-audio-client-libraries \
  wireplumber \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  libgstreamer-plugins-good1.0-dev \
  libgstreamer-plugins-bad1.0-dev \
  libclang-dev \
  pkg-config

# Enable PipeWire user services
echo ""
echo "--- Enabling PipeWire services ---"
systemctl --user enable --now pipewire pipewire-pulse wireplumber

# Verify PipeWire is running
echo ""
echo "--- Verifying PipeWire ---"
if pw-cli info 0 >/dev/null 2>&1; then
  echo "PipeWire is running."
else
  echo "WARNING: PipeWire does not appear to be running."
  echo "You may need to log out and back in, or run: systemctl --user start pipewire"
fi

# Check Rust
echo ""
echo "--- Checking Rust toolchain ---"
if command -v rustc >/dev/null 2>&1; then
  echo "Rust $(rustc --version) is installed."
else
  echo "Rust is not installed. Installing via rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
  echo "Rust $(rustc --version) installed."
fi

echo ""
echo "=== Setup complete ==="
