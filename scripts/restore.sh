#!/bin/sh
# restore.sh - bring smart_remarkable back after a reMarkable OS update.
#
# OS updates swap the root partition, so anything outside /home/root (XOVI's
# hook into xochitl, systemd units, loaded kernel modules) is gone afterwards.
# The binary in /home/root survives. Run this once after each update:
#
#   ssh root@<device-ip> ./restore.sh
#
# Steps: read IMG_VERSION, check the binary, check uinput support for this OS
# version (Paper Pro), re-run XOVI's start script (or preload xovi.so), start the app if it isn't
# already running. Safe to run repeatedly: a second run changes nothing.
#
# Overrides (environment):
#   SR_DIR       directory holding the binary and sr.log  (default: /home/root)
#   SR_ENV       env file with API keys, loaded before start (default: $SR_DIR/.env)
#   SR_ARGS      arguments passed to the app               (default: --select-mode)
#   XOVI_START   XOVI start script                         (default: $HOME/xovi/start)
#   XOVI_SO      used when there is no start script: preloaded into xochitl
#                via a /run systemd drop-in        (default: $HOME/xovi/xovi.so)
#   SKIP_XOVI=1  don't touch XOVI
#   XOVI_TIMEOUT seconds to wait for xochitl after xovi/start (default: 30)
#   SR_START_CHECK seconds after launch to confirm the app is still up (default: 2)
#   SR_OS_RELEASE, SR_HWREVISION, SR_PROC  test hooks for /etc/os-release,
#                /etc/hwrevision and /proc

set -u

SR_DIR="${SR_DIR:-/home/root}"
SR_BIN="$SR_DIR/smart_remarkable"
SR_LOG="$SR_DIR/sr.log"
SR_ENV="${SR_ENV:-$SR_DIR/.env}"
SR_ARGS="${SR_ARGS:---select-mode}"
XOVI_START="${XOVI_START:-${HOME:-/home/root}/xovi/start}"
XOVI_SO="${XOVI_SO:-${HOME:-/home/root}/xovi/xovi.so}"
XOVI_DROPIN_DIR="${XOVI_DROPIN_DIR:-/run/systemd/system/xochitl.service.d}"
SKIP_XOVI="${SKIP_XOVI:-0}"
XOVI_TIMEOUT="${XOVI_TIMEOUT:-30}"
SR_START_CHECK="${SR_START_CHECK:-2}"
OS_RELEASE="${SR_OS_RELEASE:-/etc/os-release}"
HWREVISION="${SR_HWREVISION:-/etc/hwrevision}"
PROC="${SR_PROC:-/proc}"

say() { printf 'restore: %s\n' "$*"; }
die() { printf 'restore: ERROR: %s\n' "$*" >&2; exit 1; }

# 1. IMG_VERSION
IMG_VERSION=""
if [ -r "$OS_RELEASE" ]; then
    IMG_VERSION=$(sed -n 's/^IMG_VERSION=//p' "$OS_RELEASE" | tr -d '"' | head -n 1)
fi
[ -n "$IMG_VERSION" ] || die "could not read IMG_VERSION from $OS_RELEASE"
SHORT_VERSION=$(printf '%s' "$IMG_VERSION" | cut -d. -f1-2)
say "reMarkable OS $IMG_VERSION"

# 2. Binary present
[ -f "$SR_BIN" ] || die "$SR_BIN is missing; redeploy it with ./build.sh <host>"
[ -x "$SR_BIN" ] || chmod +x "$SR_BIN" || die "cannot make $SR_BIN executable"
say "binary ok: $SR_BIN"

# 3. uinput (Paper Pro only, mirrors setup_uinput() in src/util.rs)
if grep -q 'ferrari 1.0' "$HWREVISION" 2>/dev/null; then
    if [ -e /dev/uinput ]; then
        say "uinput ok: /dev/uinput exists"
    elif grep -q '^uinput ' "$PROC/modules" 2>/dev/null; then
        say "uinput ok: module already loaded"
    elif grep -q "rmpp/uinput-$SHORT_VERSION.ko" "$SR_BIN"; then
        say "uinput ok: bundled module for $SHORT_VERSION (the app loads it)"
    else
        die "no bundled uinput module for reMarkable OS $SHORT_VERSION (IMG_VERSION=$IMG_VERSION). The pen can't draw on this version, so the app was not started. Report this IMG_VERSION so a module can be added under utils/rmpp/."
    fi
else
    say "uinput check skipped (not a Paper Pro)"
fi

# 4. XOVI (re-enables the llmbutton LLM/Draw buttons)
xovi_active() {
    for pid in $(pidof xochitl 2>/dev/null); do
        if tr '\0' '\n' 2>/dev/null < "$PROC/$pid/environ" | grep -q '^LD_PRELOAD=.*xovi'; then
            return 0
        fi
    done
    return 1
}
# xochitl restarts after XOVI is hooked in; wait for it before the app.
wait_xochitl() {
    waited=0
    until pidof xochitl >/dev/null 2>&1; do
        [ "$waited" -ge "$XOVI_TIMEOUT" ] && die "xochitl did not come back within ${XOVI_TIMEOUT}s after $1; app not started. Check 'systemctl status xochitl'."
        sleep 1
        waited=$((waited + 1))
    done
    say "xochitl running again"
}
if [ "$SKIP_XOVI" = "1" ]; then
    say "xovi skipped (SKIP_XOVI=1)"
elif xovi_active; then
    say "xovi ok: already loaded into xochitl"
elif [ -x "$XOVI_START" ]; then
    say "starting xovi: $XOVI_START"
    "$XOVI_START" || die "$XOVI_START failed"
    wait_xochitl "$XOVI_START"
elif [ -f "$XOVI_SO" ]; then
    # No start script (only xovi.so + extensions.d): preload it the same way,
    # via a systemd drop-in under /run so nothing in /etc is changed.
    say "starting xovi: preloading $XOVI_SO into xochitl"
    mkdir -p "$XOVI_DROPIN_DIR" || die "cannot create $XOVI_DROPIN_DIR"
    printf '[Service]\nEnvironment="LD_PRELOAD=%s"\n' "$XOVI_SO" > "$XOVI_DROPIN_DIR/xovi.conf" \
        || die "cannot write $XOVI_DROPIN_DIR/xovi.conf"
    systemctl daemon-reload || die "systemctl daemon-reload failed"
    systemctl restart xochitl || die "systemctl restart xochitl failed"
    wait_xochitl "the xovi preload"
    xovi_active || die "xochitl restarted but xovi is not loaded; check 'systemctl cat xochitl'"
else
    die "XOVI not found: no start script at $XOVI_START and no $XOVI_SO (set XOVI_START=/path/to/xovi/start or SKIP_XOVI=1)"
fi

# 5. App
# "smart_remarkable" is 16 chars and the kernel truncates process names to
# 15, so match on argv[0] in /proc instead of relying on pidof.
app_pids() {
    for d in "$PROC"/[0-9]*; do
        arg0=$(tr '\0' '\n' 2>/dev/null < "$d/cmdline" | head -n 1)
        [ "${arg0##*/}" = smart_remarkable ] && printf '%s ' "${d##*/}"
    done
}
pids=$(app_pids)
if [ -n "$pids" ]; then
    say "app ok: already running (pid ${pids% })"
else
    cd "$SR_DIR" || die "cannot cd to $SR_DIR"
    # API keys (OPENAI_API_KEY, GEMINI_API_KEY, ...) live in $SR_ENV.
    if [ -f "$SR_ENV" ]; then
        set -a
        # shellcheck disable=SC1090
        . "$SR_ENV" || die "cannot read $SR_ENV"
        set +a
        say "env loaded: $SR_ENV"
    fi
    # SR_ARGS is intentionally split into separate arguments.
    # shellcheck disable=SC2086
    nohup "$SR_BIN" $SR_ARGS > "$SR_LOG" 2>&1 &
    app_pid=$!
    # Catch an app that dies right away (bad API key, missing uinput, ...).
    sleep "$SR_START_CHECK"
    if [ -z "$(app_pids)" ]; then
        printf 'restore: ERROR: app exited within %ss of starting. Last lines of %s:\n' "$SR_START_CHECK" "$SR_LOG" >&2
        tail -n 20 "$SR_LOG" >&2
        exit 1
    fi
    say "app started (pid $app_pid), logging to $SR_LOG"
fi
