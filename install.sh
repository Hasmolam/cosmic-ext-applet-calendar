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

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd || pwd)"
PACKAGE_BIN="${REPO_ROOT}/cosmic-ext-applet-calendar"
PACKAGE_TIME_BIN="${REPO_ROOT}/cosmic-applet-time"
RELEASE_BIN="${REPO_ROOT}/target/release/cosmic-ext-applet-calendar"
RELEASE_TIME_BIN="${REPO_ROOT}/target/release/cosmic-applet-time"
GITHUB_REPO="Hasmolam/cosmic-ext-applet-calendar"
RAW_BASE_URL="https://raw.githubusercontent.com/${GITHUB_REPO}/main"

REPLACE_CLOCK=0

# Parse command line options
for arg in "$@"; do
    case "$arg" in
        --replace-clock|-r)
            REPLACE_CLOCK=1
            ;;
        --help|-h)
            echo "Usage: ./install.sh [OPTIONS]"
            echo "Options:"
            echo "  --replace-clock, -r   Also replace default system clock (~/.local/bin/cosmic-applet-time)"
            echo "  --help, -h            Show this help message"
            exit 0
            ;;
    esac
done

echo -e "${BLUE}${BOLD}=== COSMIC Calendar Applet Installer ===${NC}\n"

# 1. Ensure target directories exist
mkdir -p "${TARGET_DIR}" "${APP_DIR}" "${ICON_DIR}"

if [[ ":${PATH}:" != *":${TARGET_DIR}:"* ]]; then
    echo -e "${YELLOW}[!] Warning:${NC} ${TARGET_DIR} is not in your PATH."
    echo -e "    Add 'export PATH=\"\$HOME/.local/bin:\$PATH\"' to your ~/.bashrc or ~/.zshrc."
fi

# 2. Check if user wants to replace system clock if not explicitly passed
if [[ "${REPLACE_CLOCK}" -eq 0 && -t 0 ]]; then
    echo -e "By default, this installs as a standalone panel applet (${BOLD}cosmic-ext-applet-calendar${NC})."
    read -r -p "Do you also want to replace the default COSMIC top-bar clock with this calendar? [y/N]: " choice || true
    case "${choice:-}" in
        [yY][eE][sS]|[yY])
            REPLACE_CLOCK=1
            ;;
        *)
            REPLACE_CLOCK=0
            ;;
    esac
    echo ""
fi

# 3. Determine installation source and install binary
INSTALLED=0

# Clean old destination binaries to prevent ETXTBSY ("Text file busy") if running
rm -f "${TARGET_BIN}"
if [[ "${REPLACE_CLOCK}" -eq 1 ]]; then
    rm -f "${TIME_COMPAT_BIN}"
fi

# Option A: Prebuilt binary in package directory
if [[ -f "${PACKAGE_BIN}" ]]; then
    echo -e "${BLUE}[*] Found binaries in release package.${NC}"
    install -m 0755 "${PACKAGE_BIN}" "${TARGET_BIN}"
    if [[ "${REPLACE_CLOCK}" -eq 1 ]]; then
        if [[ -f "${PACKAGE_TIME_BIN}" ]]; then
            install -m 0755 "${PACKAGE_TIME_BIN}" "${TIME_COMPAT_BIN}"
        else
            install -m 0755 "${PACKAGE_BIN}" "${TIME_COMPAT_BIN}"
        fi
    fi
    INSTALLED=1

# Option B: Local compiled release binary in repo
elif [[ -f "${RELEASE_BIN}" ]]; then
    echo -e "${BLUE}[*] Found locally compiled release binary.${NC}"
    install -m 0755 "${RELEASE_BIN}" "${TARGET_BIN}"
    if [[ "${REPLACE_CLOCK}" -eq 1 ]]; then
        if [[ -f "${RELEASE_TIME_BIN}" ]]; then
            install -m 0755 "${RELEASE_TIME_BIN}" "${TIME_COMPAT_BIN}"
        else
            install -m 0755 "${RELEASE_BIN}" "${TIME_COMPAT_BIN}"
        fi
    fi
    INSTALLED=1

# Option C: Build from source if cargo is available
elif command -v cargo &>/dev/null && [[ -f "${REPO_ROOT}/Cargo.toml" ]]; then
    echo -e "${BLUE}[*] Building cosmic-ext-applet-calendar from source...${NC}"
    (cd "${REPO_ROOT}" && cargo build --release)
    install -m 0755 "${RELEASE_BIN}" "${TARGET_BIN}"
    if [[ "${REPLACE_CLOCK}" -eq 1 ]]; then
        if [[ -f "${RELEASE_TIME_BIN}" ]]; then
            install -m 0755 "${RELEASE_TIME_BIN}" "${TIME_COMPAT_BIN}"
        else
            install -m 0755 "${RELEASE_BIN}" "${TIME_COMPAT_BIN}"
        fi
    fi
    INSTALLED=1

# Option D: Download latest pre-built binary from GitHub Releases
else
    echo -e "${BLUE}[*] Downloading latest prebuilt binary from GitHub Releases...${NC}"
    DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/latest/download/cosmic-ext-applet-calendar"
    TMP_BIN=$(mktemp)
    if command -v curl &>/dev/null; then
        curl -fsSL "${DOWNLOAD_URL}" -o "${TMP_BIN}" || {
            echo -e "${RED}[x] Failed to download prebuilt binary.${NC}"
            rm -f "${TMP_BIN}"
            exit 1
        }
    elif command -v wget &>/dev/null; then
        wget -qO "${TMP_BIN}" "${DOWNLOAD_URL}" || {
            echo -e "${RED}[x] Failed to download prebuilt binary.${NC}"
            rm -f "${TMP_BIN}"
            exit 1
        }
    else
        echo -e "${RED}[x] Neither curl nor wget found.${NC}"
        rm -f "${TMP_BIN}"
        exit 1
    fi
    install -m 0755 "${TMP_BIN}" "${TARGET_BIN}"
    rm -f "${TMP_BIN}"

    if [[ "${REPLACE_CLOCK}" -eq 1 ]]; then
        TIME_URL="https://github.com/${GITHUB_REPO}/releases/latest/download/cosmic-applet-time"
        TMP_TIME=$(mktemp)
        if command -v curl &>/dev/null && curl -fsSL "${TIME_URL}" -o "${TMP_TIME}"; then
            install -m 0755 "${TMP_TIME}" "${TIME_COMPAT_BIN}"
        elif command -v wget &>/dev/null && wget -qO "${TMP_TIME}" "${TIME_URL}"; then
            install -m 0755 "${TMP_TIME}" "${TIME_COMPAT_BIN}"
        else
            install -m 0755 "${TARGET_BIN}" "${TIME_COMPAT_BIN}"
        fi
        rm -f "${TMP_TIME}"
    fi
    INSTALLED=1
fi

# 4. Install desktop entry and symbolic icon (supports local repo or piped curl)
DESKTOP_SRC="${REPO_ROOT}/data/io.github.hasmolam.cosmic-ext-applet-calendar.desktop"
ICON_SRC="${REPO_ROOT}/data/icons/scalable/apps/io.github.hasmolam.cosmic-ext-applet-calendar-symbolic.svg"

if [[ -f "${DESKTOP_SRC}" ]]; then
    cp "${DESKTOP_SRC}" "${APP_DIR}/"
else
    echo -e "${BLUE}[*] Downloading desktop file from GitHub...${NC}"
    if command -v curl &>/dev/null; then
        curl -fsSL "${RAW_BASE_URL}/data/io.github.hasmolam.cosmic-ext-applet-calendar.desktop" -o "${APP_DIR}/io.github.hasmolam.cosmic-ext-applet-calendar.desktop" || true
    elif command -v wget &>/dev/null; then
        wget -qO "${APP_DIR}/io.github.hasmolam.cosmic-ext-applet-calendar.desktop" "${RAW_BASE_URL}/data/io.github.hasmolam.cosmic-ext-applet-calendar.desktop" || true
    fi
fi

if [[ -f "${ICON_SRC}" ]]; then
    cp "${ICON_SRC}" "${ICON_DIR}/"
else
    echo -e "${BLUE}[*] Downloading applet icon from GitHub...${NC}"
    if command -v curl &>/dev/null; then
        curl -fsSL "${RAW_BASE_URL}/data/icons/scalable/apps/io.github.hasmolam.cosmic-ext-applet-calendar-symbolic.svg" -o "${ICON_DIR}/io.github.hasmolam.cosmic-ext-applet-calendar-symbolic.svg" || true
    elif command -v wget &>/dev/null; then
        wget -qO "${ICON_DIR}/io.github.hasmolam.cosmic-ext-applet-calendar-symbolic.svg" "${RAW_BASE_URL}/data/icons/scalable/apps/io.github.hasmolam.cosmic-ext-applet-calendar-symbolic.svg" || true
    fi
fi

# 5. Refresh desktop and icon database caches
if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi

# 6. Restart panel and summarize
if [[ "${INSTALLED}" -eq 1 ]]; then
    echo -e "\n${BLUE}[*] Restarting cosmic-panel to register applet...${NC}"
    killall cosmic-panel 2>/dev/null || true
    echo -e "${GREEN}${BOLD}✓ Success!${NC} COSMIC Calendar Applet is installed."
    echo -e "  - Binary installed to: ${TARGET_BIN}"
    echo -e "  - Available in: COSMIC Settings → Panel → Applets (search 'Calendar & Agenda')"
    if [[ "${REPLACE_CLOCK}" -eq 1 ]]; then
        echo -e "  - System clock replacement active at: ${TIME_COMPAT_BIN}"
    else
        echo -e "  - System clock untouched. (To replace clock, re-run with --replace-clock)"
    fi
    echo -e "  - To revert anytime, run: ./uninstall.sh\n"
fi
