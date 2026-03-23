#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${HOME}/.local/bin"
CONFIG_DIR="${HOME}/.config/audiomock"
SERVICE_DIR="${HOME}/.config/systemd/user"

echo "=== audiomock: uninstall ==="

# Stop and disable service
if systemctl --user is-active audiomockd &>/dev/null; then
    echo "Stopping audiomockd service..."
    systemctl --user stop audiomockd
fi

if systemctl --user is-enabled audiomockd &>/dev/null; then
    echo "Disabling audiomockd service..."
    systemctl --user disable audiomockd
fi

# Remove service file
if [ -f "${SERVICE_DIR}/audiomockd.service" ]; then
    echo "Removing systemd service..."
    rm "${SERVICE_DIR}/audiomockd.service"
    systemctl --user daemon-reload
fi

# Remove binaries
for bin in audiomockd audiomock; do
    if [ -f "${INSTALL_DIR}/${bin}" ]; then
        echo "Removing ${INSTALL_DIR}/${bin}..."
        rm "${INSTALL_DIR}/${bin}"
    fi
done

# Remove socket
SOCKET="${XDG_RUNTIME_DIR:-/tmp}/audiomockd.sock"
if [ -S "${SOCKET}" ]; then
    echo "Removing socket ${SOCKET}..."
    rm "${SOCKET}"
fi

echo ""
echo "=== Uninstalled ==="
echo "  Config preserved at: ${CONFIG_DIR}/config.toml"
echo "  To remove config:    rm -rf ${CONFIG_DIR}"
