#!/usr/bin/env bash
# Wave XLR for Linux: install for the current user. Builds from source (needs Rust), asks for sudo once for the USB rule.
#   ./install.sh               install or update
#   ./install.sh --uninstall   remove everything (add --purge to delete the saved levels too)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
bin="$HOME/.local/bin"
rule=/etc/udev/rules.d/70-wavexlr.rules
wp_conf="$config/wireplumber/wireplumber.conf.d/51-wavexlr.conf"
wp_lua="$data/wireplumber/scripts/wavedevicefix.lua"
restart_wireplumber() {
    systemctl --user restart wireplumber 2>/dev/null || true
}

stop_running() {
    pkill -x wavexlr 2>/dev/null || true
    pkill -f "[p]ython.* -m wavexlr" 2>/dev/null || true
}

if [[ "${1:-}" == "--uninstall" ]]; then
    systemctl --user disable --now wavexlr.service 2>/dev/null || true
    stop_running
    rm -rf "$data/wavexlr" "$bin/wavexlr" "$data/applications/wavexlr.desktop" \
        "$data/icons/hicolor/scalable/apps/wavexlr.svg" "$config/autostart/wavexlr.desktop" \
        "$config/systemd/user/wavexlr.service"
    for f in "$wp_conf" "$wp_lua"; do
        if grep -q "Wave XLR for Linux" "$f" 2>/dev/null; then
            rm -f "$f"
            [[ -f "$f.wavexlr-backup" ]] && mv "$f.wavexlr-backup" "$f"
        fi
    done
    restart_wireplumber
    sudo rm -f "$rule" && sudo udevadm control --reload
    [[ "${2:-}" == "--purge" ]] && rm -rf "$config/wavexlr"
    echo "Wave XLR for Linux removed."
    exit 0
fi

missing=""
command -v cargo >/dev/null || missing="Rust (cargo)"
command -v amixer >/dev/null || missing="$missing alsa-utils"
command -v parecord >/dev/null || missing="$missing libpulse/pulseaudio-utils (for the level meter)"
pkg-config --exists alsa 2>/dev/null || missing="$missing alsa-lib headers"
if [[ -n "$missing" ]]; then
    echo "Missing:$missing" >&2
    if command -v pacman >/dev/null; then echo "Install with: sudo pacman -S rust alsa-lib alsa-utils libpulse" >&2
    elif command -v apt >/dev/null; then echo "Install with: sudo apt install cargo libasound2-dev alsa-utils pulseaudio-utils pkg-config" >&2
    elif command -v dnf >/dev/null; then echo "Install with: sudo dnf install cargo alsa-lib-devel alsa-utils pulseaudio-utils pkgconf" >&2
    fi
    exit 1
fi

echo "Building (the first build takes a few minutes) ..."
(cd "$here" && cargo build --release --locked 2>&1 | tail -n 3)

stop_running
sleep 1
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/scalable/apps" "$config/autostart" "$config/systemd/user"
rm -rf "$data/wavexlr"   # leftovers of the first (Python) version
install -m 755 "$here/target/release/wavexlr" "$bin/wavexlr"
install -m 644 "$here/assets/wavexlr.svg" "$data/icons/hicolor/scalable/apps/wavexlr.svg"
# Autostart and the service need the full path: ~/.local/bin is not always in the login PATH.
for pair in "wavexlr.desktop:$data/applications/wavexlr.desktop" \
    "wavexlr-autostart.desktop:$config/autostart/wavexlr.desktop" \
    "wavexlr.service:$config/systemd/user/wavexlr.service"; do
    sed "s|^Exec=wavexlr|Exec=$bin/wavexlr|; s|^ExecStart=wavexlr|ExecStart=$bin/wavexlr|" \
        "$here/packaging/${pair%%:*}" >"${pair#*:}"
    chmod 644 "${pair#*:}"
done
# WirePlumber: microphone fix and plain device names. An older copy of the same fix is kept as a backup.
mkdir -p "$(dirname "$wp_conf")" "$(dirname "$wp_lua")"
wp_changed=0
for pair in "$here/packaging/wireplumber/51-wavexlr.conf:$wp_conf" "$here/packaging/wireplumber/wavedevicefix.lua:$wp_lua"; do
    src="${pair%%:*}"; dst="${pair#*:}"
    if [[ -f "$dst" ]] && ! grep -q "Wave XLR for Linux" "$dst"; then cp -n "$dst" "$dst.wavexlr-backup"; fi
    cmp -s "$src" "$dst" || wp_changed=1
    install -m 644 "$src" "$dst"
done
# Refresh the icon caches, so the new icon shows in the title bar and the task bar right away.
touch "$data/icons/hicolor"
gtk-update-icon-cache -q -f -t "$data/icons/hicolor" 2>/dev/null || true
kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
sudo install -m 644 "$here/packaging/70-wavexlr.rules" "$rule"
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=usb --attr-match=idVendor=0fd9

if [[ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ]]; then
    if [[ "$wp_changed" == 1 ]]; then
        echo "Restarting the audio session manager (sound pauses for a second; restart apps like Spotify afterwards) ..."
        restart_wireplumber
        sleep 2
    fi
    nohup "$bin/wavexlr" --hidden >/dev/null 2>&1 &
    echo "Installed and running in the tray. It also starts at every login."
else
    echo "Installed. Start it with: wavexlr   (it also starts at every login)"
fi
case ":$PATH:" in *":$bin:"*) ;; *) echo "Note: $bin is not in your PATH." ;; esac
