#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

INSTALL_DIR="${HOME}/.local/bin"
CONFIG_DIR="${HOME}/.config/audiomock"
SERVICE_DIR="${HOME}/.config/systemd/user"

echo "=== audiomock: install ==="

# Build release binaries
echo "Building release binaries..."
cargo build --release --manifest-path "${PROJECT_ROOT}/Cargo.toml" -p audiomockd -p audiomock

# Install binaries
echo "Installing binaries to ${INSTALL_DIR}..."
mkdir -p "${INSTALL_DIR}"
cp "${PROJECT_ROOT}/target/release/audiomockd" "${INSTALL_DIR}/audiomockd"
cp "${PROJECT_ROOT}/target/release/audiomock" "${INSTALL_DIR}/audiomock"
chmod +x "${INSTALL_DIR}/audiomockd" "${INSTALL_DIR}/audiomock"

# Install default config if not present
mkdir -p "${CONFIG_DIR}"
if [ ! -f "${CONFIG_DIR}/config.toml" ]; then
    echo "Installing default config to ${CONFIG_DIR}/config.toml..."
    cp "${PROJECT_ROOT}/config.example.toml" "${CONFIG_DIR}/config.toml"
else
    echo "Config already exists at ${CONFIG_DIR}/config.toml (not overwriting)"
fi

# Install systemd service
echo "Installing systemd user service..."
mkdir -p "${SERVICE_DIR}"
cp "${SCRIPT_DIR}/audiomockd.service" "${SERVICE_DIR}/audiomockd.service"
systemctl --user daemon-reload

echo ""
echo "=== Installed ==="
echo "  Binaries:  ${INSTALL_DIR}/audiomockd, ${INSTALL_DIR}/audiomock"
echo "  Config:    ${CONFIG_DIR}/config.toml"
echo "  Service:   ${SERVICE_DIR}/audiomockd.service"
echo ""
echo "To start now:           systemctl --user start audiomockd"
echo "To enable on login:     systemctl --user enable audiomockd"
echo "To start and enable:    systemctl --user enable --now audiomockd"
echo "To check status:        systemctl --user status audiomockd"
echo "To view logs:           journalctl --user -u audiomockd -f"
