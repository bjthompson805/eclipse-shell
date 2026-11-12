#!/bin/sh
# Installs eclipse-shell's root helper and boot script. Run as root:
#
#   sudo system/install.sh [user]
#
# The user (default: whoever ran sudo) may then run the helper, and only the
# helper, without a password. Charge-limit settings carry over from Ryoku's
# helper if it was set up, and Ryoku's own power boot scripts are turned off
# so the two don't both apply settings at boot.
#
# SPDX-License-Identifier: BSD-2-Clause
set -eu

[ "$(id -u)" = 0 ] || { echo "run this as root (sudo $0)" >&2; exit 1; }
user=${1:-${SUDO_USER:-${DOAS_USER:-}}}
[ -n "$user" ] && id "$user" >/dev/null 2>&1 || { echo "name the desktop user: $0 <user>" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)

install -o root -g wheel -m 755 "$here/eclipse-priv" /usr/local/libexec/eclipse-priv
install -o root -g wheel -m 755 "$here/rc.d/eclipse_power" /usr/local/etc/rc.d/eclipse_power

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

for old in ryoku_charge_limit ryoku_power; do
	if [ "$(sysrc -n "${old}_enable" 2>/dev/null)" = YES ]; then
		sysrc -q "${old}_enable=NO" >/dev/null
		echo "turned off $old (eclipse_power takes over)"
	fi
done
sysrc -q eclipse_power_enable=YES >/dev/null
echo "installed eclipse-priv and eclipse_power for $user"
