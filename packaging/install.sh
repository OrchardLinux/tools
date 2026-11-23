#!/bin/sh
# Install @NAME@'s installer and start it. It installs @NAME@ on the iPad, then
# puts Orchard (the launcher) on this computer.
#   curl -fsSL @BASE@/install.sh | sh
# The command-line installer instead (no screen, scripts):
#   curl -fsSL @BASE@/install.sh | sh -s -- --cli
# It is the release's install-cli.sh; this script only hands over to it, also
# on Linux without a display (an SSH session).
# macOS: into ~/Applications (curl sets no quarantine flag, so it opens without
# Gatekeeper's prompt). Linux: into ~/.local/bin, with a desktop entry and icons
# in ~/.local/share. Nothing needs root, except the window's libraries and
# usbmuxd on Linux if they are missing.
# (A template: packaging/stamp.py fills in the names and this release's URL,
# so it downloads from the release it came with, a pre-release too.)
set -eu
BASE=${ORCHARD_RELEASE_URL:-@BASE@}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

say() { printf '%s\n' "$*"; }
fetch() { curl -fL --progress-bar -o "$2" "$BASE/$1" || { say "Couldn't download $1."; exit 1; }; }

cli() {
    fetch install-cli.sh "$work/install-cli.sh"
    sh "$work/install-cli.sh"
    exit $?
}
case "${1:-}" in
    --cli) cli ;;
    "") ;;
    *) say "Unknown option $1 (--cli installs the command-line installer)."; exit 1 ;;
esac
if [ "$(uname -s)" = Linux ] && [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    say "No display here: installing the command-line installer instead."
    cli
fi

case "$(uname -s)" in
Darwin)
    fetch Install-@SLUG@-macOS.zip "$work/app.zip"
    mkdir -p "$HOME/Applications"
    rm -rf "$HOME/Applications/Install @NAME@.app"
    ditto -x -k "$work/app.zip" "$HOME/Applications"
    say "Installed Install @NAME@ in ~/Applications."
    open "$HOME/Applications/Install @NAME@.app"
    ;;
Linux)
    case "$(uname -m)" in
        x86_64|amd64) arch=x86_64 ;;
        aarch64|arm64) arch=aarch64 ;;
        *) say "This computer ($(uname -m)) isn't supported."; exit 1 ;;
    esac
    fetch "Install-@SLUG@-linux-$arch.tar.gz" "$work/app.tar.gz"
    tar -C "$work" -xzf "$work/app.tar.gz"
    bin="$HOME/.local/bin"
    share="${XDG_DATA_HOME:-$HOME/.local/share}"
    exe="$bin/install-@SLUG_LOWER@"
    mkdir -p "$bin" "$share/applications" "$share/icons"
    install -m 755 "$work/install-@SLUG_LOWER@" "$exe"
    # The menu entry by full path: ~/.local/bin may not be on PATH.
    sed "s|^Exec=.*|Exec=$exe|" "$work/applications/install-@SLUG_LOWER@.desktop" > "$share/applications/install-@SLUG_LOWER@.desktop"
    cp -R "$work/icons/hicolor" "$share/icons/"
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$share/applications" >/dev/null 2>&1 || true
    say "Installed $exe."
    # What the installer needs from the system: the window's libraries (an
    # Omarchy desktop has them) and usbmuxd (the iPad's connection while splitting).
    need=""
    ldd "$exe" 2>/dev/null | grep -q "libxkbcommon-x11.*not found" && need="$need libxkbcommon-x11"
    ldd "$exe" 2>/dev/null | grep -q "libxkbcommon\.so.*not found" && need="$need libxkbcommon"
    ldd "$exe" 2>/dev/null | grep -q "libxcb\.so.*not found" && need="$need libxcb"
    command -v usbmuxd >/dev/null 2>&1 || need="$need usbmuxd"
    if [ -n "$need" ]; then
        if command -v pacman >/dev/null 2>&1; then
            say "Installing$need (asks for your password)…"
            sudo pacman -S --needed --noconfirm $need || say "Install them yourself: sudo pacman -S$need"
        else
            say "Install these with your package manager:$need"
        fi
    fi
    (setsid "$exe" >/dev/null 2>&1 &) || "$exe" &
    ;;
*)
    say "This system ($(uname -s)) isn't supported."
    exit 1
    ;;
esac
