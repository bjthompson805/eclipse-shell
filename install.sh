#!/bin/sh
# Installs eclipse-shell. Run it as the desktop user, from the checkout; the
# root steps go through doas or sudo. Safe to run again: it's also how an
# updated checkout reaches the machine.
#
#   ./install.sh              packages, build, root helper, restart the shell
#   ./install.sh --no-system  skip packages and the root helper (build only)
#
# The root helper (eclipse-priv) does the few things the shell needs root for:
# power profiles, the battery charge limit, the Wi-Fi radio and, on M5 Macs,
# joining networks through the N1 Wi-Fi device (eclipse-n1wifi). The user may
# run it, and only it, without a password.
#
# It also sets up the session: D-Bus, and a ConsoleKit session for every
# console login, which polkit needs before it lets a file manager mount a
# drive. Start the desktop with `dbus-run-session start-hyprland`.
#
# SPDX-License-Identifier: BSD-2-Clause
set -eu

repo=$(cd "$(dirname "$0")" && pwd)
system=1
for arg; do
	case "$arg" in
	--no-system) system=0 ;;
	*) echo "usage: $0 [--no-system]" >&2; exit 2 ;;
	esac
done
[ "$(id -u)" != 0 ] || { echo "install.sh: run this as your desktop user, not root" >&2; exit 1; }
user=$(id -un)
say() { printf '  %s\n' "$*"; }

if command -v doas >/dev/null 2>&1; then as_root=doas; else as_root=sudo; fi

# ---- packages ----------------------------------------------------------------

packages="gtk4 adwaita-icon-theme gtk4-layer-shell rust curl wallust papirus-icon-theme nerd-fonts-jetbrainsmono pipewire pipewire-spa-oss-ng wireplumber webp-pixbuf-loader awww hyprsunset mpv mpv-mpris yt-dlp dbus consolekit2 polkit bsdisks"
if [ $system -eq 1 ]; then
	# Ask for the password once, up front, so a missing tty fails here.
	if [ "$as_root" = sudo ] && ! sudo -v; then
		echo "install.sh: sudo needs a password; run this from a terminal" >&2
		exit 1
	fi
	missing=
	for p in $packages; do
		pkg info -q "$p" 2>/dev/null || missing="$missing $p"
	done
	if [ -n "$missing" ]; then
		say "installing packages:$missing"
		# shellcheck disable=SC2086
		$as_root pkg install -y $missing
	fi
fi

# ---- fonts pkg doesn't carry -------------------------------------------------

if ! fc-list 'Material Symbols Rounded' family | grep -q .; then
	fonts="$HOME/.local/share/fonts/eclipse-shell"
	mkdir -p "$fonts"
	say "fetching Material Symbols Rounded"
	fetch -q -o "$fonts/MaterialSymbolsRounded.ttf" \
		'https://raw.githubusercontent.com/google/material-design-icons/master/variablefont/MaterialSymbolsRounded%5BFILL,GRAD,opsz,wght%5D.ttf' ||
		say "could not fetch Material Symbols Rounded; glyphs will be missing"
	fc-cache -f "$fonts" >/dev/null 2>&1 || :
fi

# ---- cursor theme ------------------------------------------------------------

# With no "default" cursor theme Hyprland draws its built-in arrow for every
# shape, so clickable widgets never show a pointer. This theme keeps that
# arrow (data/cursors/default, its pixels from Hyprland's XCursorManager.cpp)
# and takes the other shapes from Adwaita. A theme already there is left alone.
cursors="$HOME/.local/share/icons/default"
if [ ! -e "$cursors/index.theme" ]; then
	mkdir -p "$cursors/cursors"
	printf '[Icon Theme]\nName=default\nComment=Hyprland arrow, Adwaita shapes\nInherits=Adwaita\n' >"$cursors/index.theme"
	cp "$repo/data/cursors/default" "$cursors/cursors/default"
	for n in left_ptr arrow top_left_arrow; do
		ln -sf default "$cursors/cursors/$n"
	done
fi

# ---- the shell ---------------------------------------------------------------

say "building eclipse-shell"
cargo install --quiet --path "$repo" --root "$HOME/.local"
# Updates (`eclipse-shell update`, the hub's Updates page) follow this
# checkout from the commit installed here.
rm -f "$HOME/.local/bin/eclipse-update"
mkdir -p "$HOME/.local/state/eclipse-shell"
printf '%s\n' "$repo" >"$HOME/.local/state/eclipse-shell/repo"
git -C "$repo" rev-parse HEAD >"$HOME/.local/state/eclipse-shell/installed" 2>/dev/null || :
# The wallpaper browser, for the launcher.
mkdir -p "$HOME/.local/share/applications"
cp "$repo/data/io.github.bjthompson805.EclipseWalls.desktop" "$HOME/.local/share/applications/"

# ---- root helper and boot script ---------------------------------------------

if [ $system -eq 1 ]; then
	say "installing the root helper"
	$as_root sh -s -- "$user" "$repo" "$HOME/.local/bin" <<'ROOT'
set -eu
user=$1
repo=$2
bin=$3
install -o root -g wheel -m 755 "$repo/system/eclipse-priv" /usr/local/libexec/eclipse-priv
# The N1 Wi-Fi ioctls (M5 Macs), which eclipse-priv runs as root. A root-owned
# copy: the one cargo built is the user's to change.
install -o root -g wheel -m 755 "$bin/eclipse-n1wifi" /usr/local/libexec/eclipse-n1wifi
install -o root -g wheel -m 755 "$repo/system/rc.d/eclipse_power" /usr/local/etc/rc.d/eclipse_power

# The sudo rule, checked before it goes live.
rule=/usr/local/etc/sudoers.d/eclipse-shell
if [ -d /usr/local/etc/sudoers.d ]; then
	printf '%s ALL=(root) NOPASSWD: /usr/local/libexec/eclipse-priv\n' "$user" >"$rule.new"
	chmod 440 "$rule.new"
	visudo -cf "$rule.new" >/dev/null
	mv -f "$rule.new" "$rule"
fi
# doas, if that's what this system uses.
if [ -f /usr/local/etc/doas.conf ] && ! grep -q 'eclipse-priv' /usr/local/etc/doas.conf; then
	printf 'permit nopass %s as root cmd /usr/local/libexec/eclipse-priv\n' "$user" >>/usr/local/etc/doas.conf
fi

# Find out whether there is a charge limit (again on every install: a newer
# kernel may have added one; only the "supported" setting changes).
/usr/local/libexec/eclipse-priv charge-limit probe || :
sysrc -q eclipse_power_enable=YES >/dev/null

# The session. Polkit lets a file manager mount a drive (through bsdisks)
# without a password only in an active local session, which ConsoleKit
# tracks; pam_ck_connector registers one for each console login.
sysrc -q dbus_enable=YES >/dev/null
if ! grep -q pam_ck_connector /etc/pam.d/login; then
	cp -p /etc/pam.d/login /etc/pam.d/login.pre-eclipse
	printf 'session\t\toptional\t/usr/local/lib/pam_ck_connector.so\n' >>/etc/pam.d/login
	echo "  added a ConsoleKit session to console logins (/etc/pam.d/login)"
fi
ROOT
fi

# ---- start it with Hyprland --------------------------------------------------

# A config that doesn't mention eclipse-shell gets a drop-in that starts it
# (with the wallpaper daemon and PipeWire it relies on) and binds its main
# keys where a stock config leaves them free. Full paths: ~/.local/bin need
# not be on the compositor's PATH. A config that already runs it is left
# alone, so a hand-written setup stays as it is.
hypr="$HOME/.config/hypr"
bin="$HOME/.local/bin/eclipse-shell"
if ! grep -rqs --exclude='*.bak*' --exclude='*.pre-*' 'eclipse-shell' "$hypr"; then
	mkdir -p "$hypr"
	if [ -f "$hypr/hyprland.conf" ] && [ ! -f "$hypr/hyprland.lua" ]; then
		cp -p "$hypr/hyprland.conf" "$hypr/hyprland.conf.pre-eclipse"
		cat >>"$hypr/hyprland.conf" <<EOF

# eclipse-shell (added by its install.sh)
exec-once = test -S "\$XDG_RUNTIME_DIR/pipewire-0" || { pipewire & sleep 1; pipewire-pulse & }
exec-once = awww-daemon
exec-once = $bin
bind = SUPER, Space, exec, $bin toggle-launcher
bind = SUPER, D, exec, $bin toggle-sidebar-left
bind = SUPER ALT, D, exec, $bin toggle-sidebar-right
bind = SUPER, Tab, exec, $bin toggle-overview
bind = SUPER, Escape, exec, $bin toggle-power
bind = SUPER, comma, exec, $bin hub
bind = SUPER, K, exec, $bin hub keybinds
bind = SUPER, W, exec, $bin wallpaper-next
bind = SUPER SHIFT, W, exec, $bin walls
EOF
		say "added eclipse-shell to Hyprland's startup (~/.config/hypr/hyprland.conf)"
	else
		cat >"$hypr/eclipse_shell.lua" <<EOF
-- eclipse-shell: start it with the session, and its main keys. Written by
-- eclipse-shell's install.sh; edit freely (install.sh won't touch it again).
local shell = "$bin"

hl.on("hyprland.start", function()
    -- no user service manager starts PipeWire, so the session does, unless
    -- its own runtime dir has one (a PipeWire started from ssh lives in
    -- another dir, out of the shell's reach); `test`, as exec reads a
    -- leading "[...]" as window rules
    hl.exec_cmd('test -S "\$XDG_RUNTIME_DIR/pipewire-0" || { pipewire & sleep 1; pipewire-pulse & }')
    hl.exec_cmd("awww-daemon")
    hl.exec_cmd(shell)
end)

hl.bind("SUPER + Space",      hl.dsp.exec_cmd(shell .. " toggle-launcher"))
hl.bind("SUPER + D",          hl.dsp.exec_cmd(shell .. " toggle-sidebar-left"))
hl.bind("SUPER + ALT + D",    hl.dsp.exec_cmd(shell .. " toggle-sidebar-right"))
hl.bind("SUPER + Tab",        hl.dsp.exec_cmd(shell .. " toggle-overview"))
hl.bind("SUPER + Escape",     hl.dsp.exec_cmd(shell .. " toggle-power"))
hl.bind("SUPER + comma",      hl.dsp.exec_cmd(shell .. " hub"))
hl.bind("SUPER + K",          hl.dsp.exec_cmd(shell .. " hub keybinds"))
hl.bind("SUPER + W",          hl.dsp.exec_cmd(shell .. " wallpaper-next"))
hl.bind("SUPER + SHIFT + W",  hl.dsp.exec_cmd(shell .. " walls"))

-- what the hub (Super + ,) changes in Hyprland: options, startup commands,
-- window rules. it writes eclipse.lua on the first change; probe for it, as
-- Hyprland shows even a pcall'd require of a missing module as a config error
if package.searchpath == nil or package.searchpath("eclipse", package.path) then
    pcall(require, "eclipse")
end
EOF
		if [ -f "$hypr/hyprland.lua" ]; then
			cp -p "$hypr/hyprland.lua" "$hypr/hyprland.lua.pre-eclipse"
			printf '\n-- eclipse-shell (added by its install.sh)\nrequire("eclipse_shell")\n' >>"$hypr/hyprland.lua"
		else
			printf -- '-- eclipse-shell (added by its install.sh)\nrequire("eclipse_shell")\n' >"$hypr/hyprland.lua"
		fi
		say "added eclipse-shell to Hyprland's startup (~/.config/hypr/eclipse_shell.lua)"
	fi
fi

# ---- restart a running shell -------------------------------------------------

if pgrep -qx -U "$user" eclipse-shell; then
	say "restarting eclipse-shell"
	pkill -x -U "$user" eclipse-shell
	sleep 0.5
	daemon -f "$HOME/.local/bin/eclipse-shell"
elif [ -n "${WAYLAND_DISPLAY:-}" ]; then
	daemon -f "$HOME/.local/bin/eclipse-shell"
fi

echo "eclipse-shell is installed at ~/.local/bin/eclipse-shell"
