#!/bin/bash
#
# Uninstallation script for solana-validator-exporter systemd service
#
# Usage: sudo ./uninstall.sh
#

set -e

# Configuration
INSTALL_DIR="/opt/solana-validator-exporter"
SERVICE_USER="solana-exporter"
SERVICE_NAME="solana-validator-exporter"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${YELLOW}=== Solana Validator Exporter Uninstallation ===${NC}"

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}Error: Please run as root (sudo ./uninstall.sh)${NC}"
    exit 1
fi

# Stop and disable service
echo -e "${YELLOW}Stopping service...${NC}"
systemctl stop "$SERVICE_NAME" 2>/dev/null || true
systemctl disable "$SERVICE_NAME" 2>/dev/null || true

# Remove systemd service file
echo -e "${YELLOW}Removing systemd service...${NC}"
rm -f "/etc/systemd/system/$SERVICE_NAME.service"
systemctl daemon-reload

# Ask before removing installation directory
read -p "Remove installation directory $INSTALL_DIR? (y/N) " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    echo -e "${YELLOW}Removing installation directory...${NC}"
    rm -rf "$INSTALL_DIR"
fi

# Ask before removing user
read -p "Remove service user $SERVICE_USER? (y/N) " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    echo -e "${YELLOW}Removing service user...${NC}"
    userdel "$SERVICE_USER" 2>/dev/null || true
fi

echo -e "${GREEN}=== Uninstallation Complete ===${NC}"




