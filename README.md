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

- FreeBSD (Linux probably works too, untested) running Hyprland with a Lua config
- `gtk4`, `gtk4-layer-shell`, Rust, `curl`
- Fonts: JetBrainsMono Nerd Font, Material Symbols Rounded
- Icon theme: Papirus-Dark
- wallust, writing `~/.cache/wallust/colors.json`

## Running

```sh
cargo run --release
```

Open the launcher, for example from a keybinding:

```sh
eclipse-shell toggle-launcher
```

Settings currently live in `src/config.rs`.

## License

BSD 2-Clause; see [LICENSE](LICENSE). The weather icons' path data comes from
Feather (MIT); see [LICENSES/Feather-MIT.txt](LICENSES/Feather-MIT.txt).
Weather data is from [Open-Meteo](https://open-meteo.com).
