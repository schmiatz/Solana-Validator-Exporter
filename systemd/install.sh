#!/bin/bash
#
# Installation script for solana-validator-exporter systemd service
#
# Usage: sudo ./install.sh
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

echo -e "${GREEN}=== Solana Validator Exporter Installation ===${NC}"

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}Error: Please run as root (sudo ./install.sh)${NC}"
    exit 1
fi

# Get the directory where this script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Check if binary exists
if [ ! -f "$PROJECT_DIR/target/release/solana-validator-exporter" ]; then
    echo -e "${RED}Error: Binary not found. Please build first with: cargo build --release${NC}"
    exit 1
fi

# Create service user if it doesn't exist
if ! id "$SERVICE_USER" &>/dev/null; then
    echo -e "${YELLOW}Creating service user: $SERVICE_USER${NC}"
    useradd --system --no-create-home --shell /bin/false "$SERVICE_USER"
fi

# Create installation directory
echo -e "${YELLOW}Creating installation directory: $INSTALL_DIR${NC}"
mkdir -p "$INSTALL_DIR"

# Copy binary
echo -e "${YELLOW}Copying binary...${NC}"
cp "$PROJECT_DIR/target/release/solana-validator-exporter" "$INSTALL_DIR/"
chmod 755 "$INSTALL_DIR/solana-validator-exporter"

# Copy or create config file
if [ -f "$PROJECT_DIR/config.yaml" ]; then
    echo -e "${YELLOW}Copying config.yaml...${NC}"
    cp "$PROJECT_DIR/config.yaml" "$INSTALL_DIR/"
elif [ -f "$PROJECT_DIR/config.example.yaml" ]; then
    echo -e "${YELLOW}Copying config.example.yaml as config.yaml...${NC}"
    cp "$PROJECT_DIR/config.example.yaml" "$INSTALL_DIR/config.yaml"
    echo -e "${RED}WARNING: Please edit $INSTALL_DIR/config.yaml with your validator details!${NC}"
fi

# Set ownership
chown -R "$SERVICE_USER:$SERVICE_USER" "$INSTALL_DIR"
chmod 600 "$INSTALL_DIR/config.yaml"  # Protect config file

# Install systemd service
echo -e "${YELLOW}Installing systemd service...${NC}"
cp "$SCRIPT_DIR/solana-validator-exporter.service" /etc/systemd/system/
systemctl daemon-reload

echo -e "${GREEN}=== Installation Complete ===${NC}"
echo ""
echo "Next steps:"
echo "  1. Edit config:        sudo nano $INSTALL_DIR/config.yaml"
echo "  2. Enable service:     sudo systemctl enable $SERVICE_NAME"
echo "  3. Start service:      sudo systemctl start $SERVICE_NAME"
echo "  4. Check status:       sudo systemctl status $SERVICE_NAME"
echo "  5. View logs:          sudo journalctl -u $SERVICE_NAME -f"
echo ""
echo "Metrics endpoint: http://localhost:9090/metrics"




