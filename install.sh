#!/usr/bin/env bash
# ==============================================================================
# COSMIC Calendar Applet (cosmic-ext-applet-calendar) - Installer
# Author: Hasan Hüseyin Yolcu <hasanhuseyinyolcu25@gmail.com>
# License: GPL-3.0-only
# ==============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
BOLD='\033[1m'
NC='\033[0m'

TARGET_DIR="${HOME}/.local/bin"
TARGET_BIN="${TARGET_DIR}/cosmic-ext-applet-calendar"
TIME_COMPAT_BIN="${TARGET_DIR}/cosmic-applet-time"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_BIN="${REPO_ROOT}/cosmic-ext-applet-calendar"
PACKAGE_TIME_BIN="${REPO_ROOT}/cosmic-applet-time"
RELEASE_BIN="${REPO_ROOT}/target/release/cosmic-ext-applet-calendar"
RELEASE_TIME_BIN="${REPO_ROOT}/target/release/cosmic-applet-time"
GITHUB_REPO="Hasmolam/cosmic-ext-applet-calendar"

echo -e "${BLUE}${BOLD}=== COSMIC Calendar Applet Installer ===${NC}\n"

# 1. Ensure target directories exist
mkdir -p "${TARGET_DIR}" "${APP_DIR}" "${ICON_DIR}"

if [[ ":${PATH}:" != *":${TARGET_DIR}:"* ]]; then
    echo -e "${YELLOW}[!] Warning:${NC} ${TARGET_DIR} is not in your PATH."
    echo -e "    Add 'export PATH=\"\$HOME/.local/bin:\$PATH\"' to your ~/.bashrc or ~/.zshrc."
fi

# 2. Determine installation source
INSTALLED=0

# Option A: Prebuilt binary in package directory
if [[ -f "${PACKAGE_BIN}" ]]; then
    echo -e "${BLUE}[*] Found binaries in release package.${NC}"
    rm -f "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    cp "${PACKAGE_BIN}" "${TARGET_BIN}"
    if [[ -f "${PACKAGE_TIME_BIN}" ]]; then
        cp "${PACKAGE_TIME_BIN}" "${TIME_COMPAT_BIN}"
    else
        ln -sf "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    fi
    chmod +x "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    INSTALLED=1

# Option B: Local compiled release binary in repo
elif [[ -f "${RELEASE_BIN}" ]]; then
    echo -e "${BLUE}[*] Found locally compiled release binary.${NC}"
    rm -f "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    cp "${RELEASE_BIN}" "${TARGET_BIN}"
    if [[ -f "${RELEASE_TIME_BIN}" ]]; then
        cp "${RELEASE_TIME_BIN}" "${TIME_COMPAT_BIN}"
    else
        ln -sf "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    fi
    chmod +x "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    INSTALLED=1

# Option C: Build from source if cargo is available
elif command -v cargo &>/dev/null && [[ -f "${REPO_ROOT}/Cargo.toml" ]]; then
    echo -e "${BLUE}[*] Building cosmic-ext-applet-calendar from source...${NC}"
    (cd "${REPO_ROOT}" && cargo build --release)
    rm -f "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    cp "${RELEASE_BIN}" "${TARGET_BIN}"
    if [[ -f "${RELEASE_TIME_BIN}" ]]; then
        cp "${RELEASE_TIME_BIN}" "${TIME_COMPAT_BIN}"
    else
        ln -sf "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    fi
    chmod +x "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    INSTALLED=1

# Option D: Download latest pre-built binary from GitHub Releases
else
    echo -e "${BLUE}[*] Downloading latest prebuilt binary from GitHub Releases...${NC}"
    DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/latest/download/cosmic-ext-applet-calendar"
    if command -v curl &>/dev/null; then
        curl -fsSL "${DOWNLOAD_URL}" -o "${TARGET_BIN}" || {
            echo -e "${RED}[x] Failed to download prebuilt binary.${NC}"
            exit 1
        }
    elif command -v wget &>/dev/null; then
        wget -qO "${TARGET_BIN}" "${DOWNLOAD_URL}" || {
            echo -e "${RED}[x] Failed to download prebuilt binary.${NC}"
            exit 1
        }
    else
        echo -e "${RED}[x] Neither curl nor wget found.${NC}"
        exit 1
    fi
    chmod +x "${TARGET_BIN}"
    ln -sf "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
    INSTALLED=1
fi

# 3. Install desktop entry and symbolic icon
if [[ -f "${REPO_ROOT}/data/io.github.hasmolam.cosmic-ext-applet-calendar.desktop" ]]; then
    cp "${REPO_ROOT}/data/io.github.hasmolam.cosmic-ext-applet-calendar.desktop" "${APP_DIR}/"
fi
if [[ -f "${REPO_ROOT}/data/icons/scalable/apps/io.github.hasmolam.cosmic-ext-applet-calendar-symbolic.svg" ]]; then
    cp "${REPO_ROOT}/data/icons/scalable/apps/io.github.hasmolam.cosmic-ext-applet-calendar-symbolic.svg" "${ICON_DIR}/"
fi

if [[ "${INSTALLED}" -eq 1 ]]; then
    echo -e "\n${BLUE}[*] Restarting cosmic-panel to activate calendar...${NC}"
    killall cosmic-panel 2>/dev/null || true
    echo -e "${GREEN}${BOLD}✓ Success!${NC} COSMIC Calendar Applet is installed."
    echo -e "  - Top bar clock is now updated with calendar events."
    echo -e "  - Also available as a standalone applet in COSMIC Settings → Panel → Applets."
    echo -e "  - To revert anytime, run: ./uninstall.sh\n"
fi
