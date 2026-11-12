# eclipse-shell

A desktop shell for [Hyprland](https://hyprland.org) on FreeBSD, written in
Rust with GTK4 and gtk4-layer-shell. It draws a frame around the screen whose
top edge carries the bar, plus an app launcher. Colours follow the wallpaper
through [wallust](https://codeberg.org/explosion-mental/wallust).

Early work in progress.

## What's there

- **Bar:** workspace pills with per-window app icons, the focused window's
  title, a clock, and a power button.
- **Launcher:** fuzzy app search; while the query is empty, a card with a
  greeting, clock, weather and a day/night horizon. Hyprland blurs the desktop
  behind it.

## Requirements

FreeBSD (Linux probably works too, untested) running Hyprland with a Lua
config. `install.sh` installs the rest: `gtk4`, `gtk4-layer-shell`, Rust,
`curl`, wallust, the Papirus icon theme, JetBrainsMono Nerd Font, PipeWire and
WirePlumber, plus Material Symbols Rounded from Google's repository.

## Install

From the checkout, as your desktop user:

```sh
./install.sh
```

It installs missing packages, builds the shell into `~/.local/bin`, installs a
small root helper for power profiles, the battery charge limit and the Wi-Fi
radio (asking for your password through doas or sudo), and restarts the shell
if it's running. Run it again after pulling to update. `./install.sh
--no-system` only builds and installs the shell.

Start `eclipse-shell` from your Hyprland autostart, and bind the launcher to a
key:

```sh
eclipse-shell toggle-launcher
```

Settings live in `~/.config/eclipse-shell/settings.json`. Edit them in the
hub, which also lists your Hyprland keybinds:

```sh
eclipse-shell hub           # settings
eclipse-shell hub keybinds  # the keybind legend
```

## License

BSD 2-Clause; see [LICENSE](LICENSE). The weather icons' path data comes from
Feather (MIT); see [LICENSES/Feather-MIT.txt](LICENSES/Feather-MIT.txt).
`protocols/hyprland-toplevel-export-v1.xml` is from
[hyprland-protocols](https://github.com/hyprwm/hyprland-protocols) (BSD
3-Clause); see
[LICENSES/hyprland-protocols-BSD-3-Clause.txt](LICENSES/hyprland-protocols-BSD-3-Clause.txt).
Weather data is from [Open-Meteo](https://open-meteo.com).
