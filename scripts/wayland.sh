#!/bin/bash
# Runs the debug build in a private headless GNOME Shell (Wayland), plays
# pointer steps, and stops it all. For bugs that only a Wayland compositor
# shows: placement, maximize, resize, client-side decorations.
#
#   scripts/wayland.sh LOG STARTUP_SECS "steps..." -- [app args...]
#   scripts/wayland.sh LOG STARTUP_SECS "steps..." -- --run PROGRAM [args...]
#
# Steps: move:X,Y click[:X,Y] dclick[:X,Y] rclick[:X,Y] press[:BUTTON]
# release[:BUTTON] wait:SECS shot:FILE.png
# windows (prints the app windows' frame rects) log:TEXT (a mark in the
# window log). X,Y are logical screen pixels; @X,Y is from the top left of the
# first app window's frame, e.g. "dclick:@300,15" on its title bar.
#
# LOG gets the app's output, LOG.windows every change of an app window's
# frame (position, size, maximized), LOG.shell the compositor's output,
# LOG.bus that of the services its D-Bus session started.
# IDEAE_WL_SIZE sets the virtual monitor (default 1920x1080), or several
# side by side: "3840x1200 1920x1200".
# IDEAE_WL_SETTINGS is added to the shell's settings, a keyfile whose groups
# are schema paths (a later key wins), e.g.
# $'[org/gnome/mutter]\ncenter-new-windows=true'.
# IDEAE_WL_EXTENSIONS="uuid ..." enables more extensions, the system's or
# copies of yours, to see whether one a user has takes part.
#
# The shell runs on its own D-Bus session, runtime dir, home and settings
# (keyfile), so nothing reaches the real desktop; the app gets no D-Bus
# session at all. Point IDEAE_STATE at a scratch file as with drive.sh.
set -u
here=$(cd "$(dirname "$0")" && pwd)

if [ "${1:-}" = "--inner" ]; then
  shift
  log=$1; startup=$2; steps=$3; shift 3; [ "${1:-}" = "--" ] && shift
  wlog=$log.windows
  : > "$wlog"
  monitors=()
  for size in ${IDEAE_WL_SIZE:-1920x1080}; do monitors+=(--virtual-monitor "$size"); done
  IDEAE_WL_LOG=$wlog gnome-shell --headless --wayland --no-x11 \
    "${monitors[@]}" --wayland-display ideae-wl \
    > "$log.shell" 2>&1 & spid=$!
  for _ in $(seq 150); do
    grep -q ' ready$' "$wlog" && break
    sleep 0.2
  done
  if ! grep -q ' ready$' "$wlog"; then
    echo "wayland.sh: the shell did not load the harness, see $log.shell" >&3
    kill $spid; wait; exit 1
  fi
  if [ "${1:-}" = "--run" ]; then
    shift; app=("$@")
  else
    app=("$here/../target/debug/ideae" "$@")
  fi
  env -u GSETTINGS_BACKEND -u XDG_CONFIG_HOME -u XDG_DATA_HOME -u XDG_CACHE_HOME \
    HOME="$REAL_HOME" DBUS_SESSION_BUS_ADDRESS=disabled: WAYLAND_DISPLAY=ideae-wl \
    "${app[@]}" > "$log" 2>&1 & apid=$!
  sleep "$startup"
  for step in $steps; do
    case $step in
      wait:*) sleep "${step#wait:}" ;;
      *)
        out=$(gdbus call --session --dest org.gnome.Shell --object-path /org/ideae/Harness \
          --method org.ideae.Harness.Step "$step" 2>&1) || { echo "wayland.sh: $step: $out" >&3; continue; }
        # ('text',) with \n escaped
        out=${out#\(\'}; out=${out%\',)}
        [ -n "$out" ] && printf '%b\n' "$out"
        ;;
    esac
  done
  kill $apid 2>/dev/null; sleep 2; kill -9 $apid 2>/dev/null
  kill $spid 2>/dev/null; wait
  exit 0
fi

dir=$(mktemp -d "${TMPDIR:-/tmp}/ideae-wl.XXXXXX")
mkdir -p "$dir"/{run,home,cache,config/glib-2.0/settings,data/gnome-shell/extensions}
chmod 700 "$dir/run"
cp -r "$here/wayland/ideae-harness@ideae" "$dir/data/gnome-shell/extensions/"
extensions="'ideae-harness@ideae'"
for uuid in ${IDEAE_WL_EXTENSIONS:-}; do
  extensions="$extensions, '$uuid'"
  if [ -d "$HOME/.local/share/gnome-shell/extensions/$uuid" ]; then
    cp -r "$HOME/.local/share/gnome-shell/extensions/$uuid" "$dir/data/gnome-shell/extensions/"
  fi
done
cat > "$dir/config/glib-2.0/settings/keyfile" <<EOF
[org/gnome/shell]
enabled-extensions=[$extensions]
disable-user-extensions=false
welcome-dialog-last-shown-version='999'

[org/gnome/desktop/interface]
enable-animations=false
enable-hot-corners=false

${IDEAE_WL_SETTINGS:-}
EOF
# Everything that has this directory in its environment is ours to stop.
cleanup() {
  for p in /proc/[0-9]*; do
    grep -qzF "XDG_RUNTIME_DIR=$dir/run" "$p/environ" 2>/dev/null && kill -9 "${p#/proc/}" 2>/dev/null
  done
  rm -rf "$dir"
}
trap cleanup EXIT
env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u XDG_SESSION_TYPE \
  -u XDG_CURRENT_DESKTOP -u GNOME_SHELL_SESSION_MODE -u DESKTOP_SESSION \
  REAL_HOME="$HOME" HOME="$dir/home" XDG_RUNTIME_DIR="$dir/run" \
  XDG_CONFIG_HOME="$dir/config" XDG_DATA_HOME="$dir/data" XDG_CACHE_HOME="$dir/cache" \
  GSETTINGS_BACKEND=keyfile \
  setsid dbus-run-session -- "$0" --inner "$@" 3>&2 2>"$1.bus"
