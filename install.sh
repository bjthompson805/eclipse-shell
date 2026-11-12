#!/bin/sh
# Installs eclipse-shell. Run it as the desktop user, from the checkout; the
# root steps go through doas or sudo. Safe to run again: it's also how an
# updated checkout reaches the machine.
#
#   ./install.sh              packages, build, root helper, restart the shell
#   ./install.sh --no-system  skip packages and the root helper (build only)
#
# The root helper (eclipse-priv) does the few things the shell needs root for:
# power profiles, the battery charge limit and the Wi-Fi radio. The user may
# run it, and only it, without a password.
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

packages="gtk4 gtk4-layer-shell rust curl wallust papirus-icon-theme nerd-fonts-jetbrainsmono pipewire wireplumber webp-pixbuf-loader awww hyprsunset"
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

# ---- the shell ---------------------------------------------------------------

say "building eclipse-shell"
cargo install --quiet --path "$repo" --root "$HOME/.local"

# ---- root helper and boot script ---------------------------------------------

if [ $system -eq 1 ]; then
	say "installing the root helper"
	$as_root sh -s -- "$user" "$repo" <<'ROOT'
set -eu
user=$1
repo=$2
install -o root -g wheel -m 755 "$repo/system/eclipse-priv" /usr/local/libexec/eclipse-priv
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

# Carry the charge limit over from Ryoku, else find out whether there is one.
if [ ! -f /var/db/eclipse-charge-limit ]; then
	if [ -f /var/db/ryoku-charge-limit ]; then
		install -o root -g wheel -m 644 /var/db/ryoku-charge-limit /var/db/eclipse-charge-limit
	else
		/usr/local/libexec/eclipse-priv charge-limit probe || :
	fi
fi

# Ryoku's power boot scripts would apply settings at boot too.
for old in ryoku_charge_limit ryoku_power; do
	if [ "$(sysrc -n "${old}_enable" 2>/dev/null)" = YES ]; then
		sysrc -q "${old}_enable=NO" >/dev/null
		echo "  turned off $old (eclipse_power takes over)"
	fi
done
sysrc -q eclipse_power_enable=YES >/dev/null
ROOT
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
if ! grep -rqs 'eclipse-shell' "$HOME/.config/hypr"; then
	echo "To start it with Hyprland, run 'eclipse-shell' from your autostart, and bind"
	echo "'eclipse-shell toggle-launcher' to a key for the launcher."
fi
