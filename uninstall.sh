#!/usr/bin/env bash
# ==============================================================================
# COSMIC Calendar Applet - Uninstaller
# Author: Hasan Hüseyin Yolcu <hasanhuseyinyolcu25@gmail.com>
# License: GPL-3.0-only
# ==============================================================================

set -euo pipefail

BLUE='\033[0;34m'
GREEN='\033[0;32m'
BOLD='\033[1m'
NC='\033[0m'

TARGET_BIN="${HOME}/.local/bin/cosmic-ext-applet-calendar"
TIME_COMPAT_BIN="${HOME}/.local/bin/cosmic-applet-time"
DESKTOP_FILE="${HOME}/.local/share/applications/io.github.hasmolam.cosmic-ext-applet-calendar.desktop"
ICON_FILE="${HOME}/.local/share/icons/hicolor/scalable/apps/io.github.hasmolam.cosmic-ext-applet-calendar-symbolic.svg"

echo -e "${BLUE}${BOLD}=== COSMIC Calendar Applet Uninstaller ===${NC}\n"

rm -f "${TARGET_BIN}" "${TIME_COMPAT_BIN}" "${DESKTOP_FILE}" "${ICON_FILE}"

echo -e "${BLUE}[*] Restarting cosmic-panel to restore default system clock...${NC}"
killall cosmic-panel 2>/dev/null || true

echo -e "${GREEN}${BOLD}✓ Success!${NC} Uninstalled cosmic-ext-applet-calendar and restored default system applet."
