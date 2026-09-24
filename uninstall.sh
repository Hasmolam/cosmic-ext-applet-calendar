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

# 1. Terminate running instances
echo -e "${BLUE}[*] Stopping running applet instances...${NC}"
killall cosmic-ext-applet-calendar 2>/dev/null || true
killall cosmic-applet-time 2>/dev/null || true

# 2. Remove installed binaries and desktop/icon files
rm -f "${TARGET_BIN}" "${TIME_COMPAT_BIN}" "${DESKTOP_FILE}" "${ICON_FILE}"

# 3. Refresh desktop and icon database caches
if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi

# 4. Restart panel to clean up applet slots
echo -e "${BLUE}[*] Restarting cosmic-panel...${NC}"
killall cosmic-panel 2>/dev/null || true

echo -e "${GREEN}${BOLD}✓ Success!${NC} Uninstalled cosmic-ext-applet-calendar and restored default system configuration.\n"
