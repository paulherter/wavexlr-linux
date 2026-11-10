#!/usr/bin/env bash
# Wave XLR for Linux: installer. One command installs it, the same command updates it.
#   curl -fsSL https://github.com/paulherter/wavexlr-linux/releases/latest/download/install.sh | bash
#   ... | bash -s -- --uninstall           remove everything (add --purge to delete the saved levels, too)
# It downloads the program, checks its checksum, sets everything up for your user and asks for your password once
# (only for the USB permission rule, and only if that rule is not installed yet).
set -euo pipefail

REPO="paulherter/wavexlr-linux"
BASE="${WAVEXLR_BASE_URL:-https://github.com/$REPO/releases/latest/download}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
bin="$HOME/.local/bin"
rule=/etc/udev/rules.d/70-wavexlr.rules
wp_conf="$config/wireplumber/wireplumber.conf.d/51-wavexlr.conf"
wp_lua="$data/wireplumber/scripts/wavedevicefix.lua"

say() { printf '%s\n' "$*"; }
die() { printf 'Wave XLR installer: %s\n' "$*" >&2; exit 1; }

[[ "$(id -u)" -ne 0 ]] || die "run it as your normal user, not as root (it asks for the password itself when needed)."

as_root() {
    if sudo -n true 2>/dev/null; then sudo "$@"
    elif command -v sudo >/dev/null; then sudo "$@"
    elif command -v pkexec >/dev/null; then pkexec "$@"
    else die "administrator rights are needed once for the USB rule, but neither sudo nor pkexec exists. Run the command as root: install -m 644 70-wavexlr.rules $rule"
    fi
}

stop_running() { pkill -x wavexlr 2>/dev/null || true; }

restart_wireplumber() { systemctl --user restart wireplumber 2>/dev/null || true; }

if [[ "${1:-}" == "--uninstall" ]]; then
    stop_running
    rm -f "$bin/wavexlr" "$data/applications/wavexlr.desktop" "$config/autostart/wavexlr.desktop" \
        "$data/icons/hicolor/scalable/apps/wavexlr.svg"
    for f in "$wp_conf" "$wp_lua"; do
        if grep -q "Wave XLR for Linux" "$f" 2>/dev/null; then
            rm -f "$f"
            [[ -f "$f.wavexlr-backup" ]] && mv "$f.wavexlr-backup" "$f"
        fi
    done
    restart_wireplumber
    if [[ -f "$rule" ]]; then
        say "One password is needed to remove the USB permission of the Wave XLR."
        as_root rm -f "$rule" && as_root udevadm control --reload || true
    fi
    [[ "${2:-}" == "--purge" ]] && rm -rf "$config/wavexlr"
    # empty folders and the caches the installer's icon and desktop refresh created, when nothing else lives there
    rmdir "$data/icons/hicolor/scalable/apps" "$data/icons/hicolor/scalable" 2>/dev/null || true
    for d in "$data/icons/hicolor" "$data/applications"; do
        if [[ -d "$d" ]] && [[ -z "$(find "$d" -mindepth 1 -not -name '*.cache' 2>/dev/null | head -1)" ]]; then
            find "$d" -maxdepth 1 -type f -name '*.cache' -delete 2>/dev/null || true
        fi
    done
    rmdir "$data/icons/hicolor" "$data/icons" "$data/applications" "$data/wireplumber/scripts" "$data/wireplumber" \
        "$config/wireplumber/wireplumber.conf.d" "$config/wireplumber" "$config/autostart" "$bin" 2>/dev/null || true
    say "Wave XLR for Linux is removed."
    exit 0
fi

[[ "$(uname -m)" == "x86_64" ]] || die "this version is for 64-bit Intel/AMD PCs (x86_64). Your machine is $(uname -m)."
if command -v curl >/dev/null; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null; then
    fetch() { wget -q "$1" -O "$2"; }
else
    die "neither curl nor wget is installed. Install curl with your package manager (Ubuntu: sudo apt install curl) and run the command again."
fi
command -v sha256sum >/dev/null || die "sha256sum is missing (coreutils)."
command -v tar >/dev/null || die "tar is missing."

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
say "Downloading Wave XLR for Linux ..."
for f in wavexlr-x86_64-linux wavexlr-support.tar.gz SHA256SUMS; do
    fetch "$BASE/$f" "$tmp/$f" || die "could not download $BASE/$f"
done
(cd "$tmp" && sha256sum --quiet -c SHA256SUMS) || die "the download is damaged (checksum does not match). Run the command again."
tar -xzf "$tmp/wavexlr-support.tar.gz" -C "$tmp"

# Program, icon, app entry, autostart
updating=0; [[ -x "$bin/wavexlr" ]] && updating=1
stop_running
sleep 1
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/scalable/apps" "$config/autostart"
install -m 755 "$tmp/wavexlr-x86_64-linux" "$bin/wavexlr"
install -m 644 "$tmp/support/wavexlr.svg" "$data/icons/hicolor/scalable/apps/wavexlr.svg"
for pair in "wavexlr.desktop:$data/applications/wavexlr.desktop" "wavexlr-autostart.desktop:$config/autostart/wavexlr.desktop"; do
    sed "s|^Exec=wavexlr|Exec=$bin/wavexlr|" "$tmp/support/${pair%%:*}" >"${pair#*:}"
    chmod 644 "${pair#*:}"
done
touch "$data/icons/hicolor" 2>/dev/null || true
gtk-update-icon-cache -q -f -t "$data/icons/hicolor" 2>/dev/null || true
command -v update-desktop-database >/dev/null && update-desktop-database -q "$data/applications" 2>/dev/null || true

# USB permission: the only step that needs the password, and only when the rule is new or changed
if ! cmp -s "$tmp/support/70-wavexlr.rules" "$rule" 2>/dev/null; then
    say "One password is needed for the USB permission of the Wave XLR."
    as_root install -m 644 "$tmp/support/70-wavexlr.rules" "$rule"
    as_root udevadm control --reload
    as_root udevadm trigger --subsystem-match=usb --attr-match=idVendor=0fd9
fi

# Audio: the microphone fix for PipeWire/WirePlumber 0.5 and plain device names
wp_note=""
if command -v wireplumber >/dev/null || command -v wpctl >/dev/null; then
    wp_version="$(wireplumber --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+' | head -1 || true)"
    wp_major="${wp_version%%.*}"; wp_minor="${wp_version##*.}"
    if [[ -n "$wp_version" && ( "$wp_major" -gt 0 || "$wp_minor" -ge 5 ) ]]; then
        mkdir -p "$(dirname "$wp_conf")" "$(dirname "$wp_lua")"
        changed=0
        for pair in "$tmp/support/51-wavexlr.conf:$wp_conf" "$tmp/support/wavedevicefix.lua:$wp_lua"; do
            src="${pair%%:*}"; dst="${pair#*:}"
            if [[ -f "$dst" ]] && ! grep -q "Wave XLR for Linux" "$dst"; then cp -n "$dst" "$dst.wavexlr-backup"; fi
            cmp -s "$src" "$dst" 2>/dev/null || changed=1
            install -m 644 "$src" "$dst"
        done
        [[ "$changed" == 1 ]] && { restart_wireplumber; wp_note="restarted"; }
    else
        wp_note="old"
    fi
fi

# Start now
started=0
if [[ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ]]; then
    args="--hidden"
    # GNOME has no tray without an extension: show the window once so there is something to see
    case "${XDG_CURRENT_DESKTOP:-}" in *GNOME*) [[ "$updating" == 1 ]] || args="" ;; esac
    (setsid nohup "$bin/wavexlr" $args >/dev/null 2>&1 &) || true
    sleep 2
    pgrep -x wavexlr >/dev/null && started=1
fi

echo
if [[ "$started" == 1 ]]; then
    case "${XDG_CURRENT_DESKTOP:-}" in
        *GNOME*) say "Done. Wave XLR is running; GNOME has no tray, so open \"Wave XLR\" from the app grid whenever you need the window." ;;
        *)       say "Done. Wave XLR is running, its tray icon appears now." ;;
    esac
else
    say "Done. Wave XLR starts at your next login (or run: $bin/wavexlr)."
fi
[[ "$wp_note" == "old" ]] && say "Note: your system has WirePlumber 0.4. The program works, but the extra microphone fix and the plain device names need WirePlumber 0.5 and are not installed. If the microphone is silent while sound plays, please open an issue: https://github.com/$REPO/issues"
case ":$PATH:" in *":$bin:"*) ;; *) true ;; esac
exit 0
