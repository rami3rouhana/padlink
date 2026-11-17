#!/bin/sh
# Installs padlink-sender on Batocera and starts it now and on every boot.
#
#   curl -fsSL https://raw.githubusercontent.com/rami3rouhana/padlink/main/deploy/batocera/install.sh | sh
#
# Run it again to update. Other uses:
#   sh install.sh ./padlink-sender     install a binary you already have (no download)
#   sh install.sh --uninstall
#   PADLINK_OPTIONS="--grab" sh install.sh        set the sender's options (kept across updates)

set -e

REPO="${PADLINK_REPO:-rami3rouhana/padlink}"
DIR=/userdata/system/padlink
SERVICE=/userdata/system/services/padlink

say() { echo "padlink: $*"; }
die() { say "$*" >&2; exit 1; }

command -v batocera-services >/dev/null 2>&1 && [ -d /userdata/system ] ||
    die "this installer is for Batocera. On other systems just run the padlink-sender binary."

if [ "$1" = "--uninstall" ]; then
    batocera-services stop padlink 2>/dev/null || true
    batocera-services disable padlink 2>/dev/null || true
    rm -rf "$DIR" "$SERVICE"
    say "removed."
    exit 0
fi

mkdir -p "$DIR" /userdata/system/services /userdata/system/logs
trap 'rm -f "$DIR/padlink-sender.new"' EXIT

if [ -n "$1" ]; then
    [ -f "$1" ] || die "no such file: $1"
    cp "$1" "$DIR/padlink-sender.new"
else
    case "$(uname -m)" in
        aarch64 | arm64) arch=aarch64 ;;
        armv7* | armv8l) arch=armv7 ;;
        x86_64) arch=x86_64 ;;
        *) die "no ready-made binary for $(uname -m); build one and pass its path to this script." ;;
    esac
    url="https://github.com/$REPO/releases/latest/download/padlink-sender-$arch"
    say "downloading $url"
    curl -fL --progress-bar -o "$DIR/padlink-sender.new" "$url" ||
        die "download failed. Check the internet connection, or that $REPO has a release."
fi
chmod +x "$DIR/padlink-sender.new"
"$DIR/padlink-sender.new" --help >/dev/null 2>&1 ||
    die "the binary does not run on this machine ($(uname -m)); nothing was changed."

# Options live in their own file so that updates keep them.
if [ -n "${PADLINK_OPTIONS+set}" ] || [ ! -f "$DIR/options" ]; then
    echo "${PADLINK_OPTIONS:-}" >"$DIR/options"
fi

cat >"$SERVICE" <<'EOF'
#!/bin/sh
# Batocera service for padlink-sender. Written by padlink's install.sh.
# Options (for example --grab or --key) go in /userdata/system/padlink/options.

BIN=/userdata/system/padlink/padlink-sender
LOG=/userdata/system/logs/padlink.log

case "$1" in
    start)
        OPTIONS="$(cat /userdata/system/padlink/options 2>/dev/null)"
        # Realtime priority keeps input forwarding ahead of whatever else the Pi is doing.
        if command -v chrt >/dev/null 2>&1; then
            chrt -f 50 "$BIN" $OPTIONS >"$LOG" 2>&1 &
        else
            "$BIN" $OPTIONS >"$LOG" 2>&1 &
        fi
        ;;
    stop)
        killall padlink-sender 2>/dev/null
        ;;
esac
EOF
chmod +x "$SERVICE"

batocera-services stop padlink 2>/dev/null || true
mv "$DIR/padlink-sender.new" "$DIR/padlink-sender"
batocera-services enable padlink
batocera-services start padlink

say "installed and running; it starts by itself on every boot."
say "controllers plugged into this machine now show up on any PC on the network that runs padlink."
say "log: /userdata/system/logs/padlink.log    options: $DIR/options"
