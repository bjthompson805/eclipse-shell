//! The hub, `eclipse-shell hub`: the shell's settings app, a window of its
//! own process. A rail of sections on the left, with a search across all of
//! them; the section's settings on the right. Each change is saved at
//! once: the shell's own settings take effect when it restarts, which the
//! footer offers, and Hyprland's (the Windows and Input pages) at once.
//! `eclipse-shell hub <page>` opens on a page, e.g. `hub keybinds`.

mod hypr;
mod keybinds;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4 as gtk;

use crate::config::{self, Settings};
use crate::theme::{self, TextStyle, BRIGHT, CREAM, DIM, FAINT, SUBTLE};
use crate::walls::ui;
use crate::{app_window, hyprland, mark, popout};
use keybinds::Group;

const APP_ID: &str = "io.github.bjthompson805.EclipseHub";
const RAIL_W: f64 = 230.0;
const CONTROL_W: f64 = 280.0;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Bar,
    Launcher,
    Frame,
    Clock,
    Fonts,
    Windows,
    Input,
    Startup,
    Keybinds,
}

impl Page {
    const ALL: [Self; 9] = [Self::Bar, Self::Launcher, Self::Frame, Self::Clock, Self::Fonts, Self::Windows, Self::Input, Self::Startup, Self::Keybinds];

    fn name(self) -> &'static str {
        match self {
            Self::Bar => "Bar",
            Self::Launcher => "Launcher",
            Self::Frame => "Frame",
            Self::Clock => "Clock & weather",
            Self::Fonts => "Fonts & scale",
            Self::Windows => "Windows",
            Self::Input => "Input",
            Self::Startup => "Startup",
            Self::Keybinds => "Keybinds",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Bar => "view_day",
            Self::Launcher => "apps",
            Self::Frame => "crop_square",
            Self::Clock => "schedule",
            Self::Fonts => "text_fields",
            Self::Windows => "select_window",
            Self::Input => "mouse",
            Self::Startup => "rocket_launch",
            Self::Keybinds => "keyboard",
        }
    }

    fn about(self) -> &'static str {
        match self {
            Self::Bar => "What the bar along the top of the screen shows.",
            Self::Launcher => "The app launcher and the card behind its empty search.",
            Self::Frame => "The frame drawn around the screen, whose top edge is the bar.",
            Self::Clock => "How times read, and where the weather comes from.",
            Self::Fonts => "Typefaces, icons, and the size of everything.",
            Self::Windows => "How Hyprland lays out and draws windows. Changes apply at once.",
            Self::Input => "Focus, pointer, touchpad and keyboard. Changes apply at once.",
            Self::Startup => "What starts when you log in.",
            Self::Keybinds => "Every shortcut in your Hyprland config, read from binds.lua and user.lua.",
        }
    }
}

enum Control {
    Switch(fn(&Settings) -> bool, fn(&mut Settings, bool)),
    /// A slider over `min..=max` in display units; the setting is the value over `per`.
    Number { get: fn(&Settings) -> f64, set: fn(&mut Settings, f64), min: f64, max: f64, per: f64, format: fn(f64) -> String },
    Text { get: fn(&Settings) -> &str, set: fn(&mut Settings, String), placeholder: &'static str },
    /// A Hyprland option, by name; read live, applied at once.
    Hypr(&'static str, Hypr),
}

enum Hypr {
    Switch,
    /// As [`Control::Number`]; written as an integer when Hyprland reports one.
    Number { min: f64, max: f64, per: f64, format: fn(f64) -> String },
    /// Labels and the values they set, as Hyprland spells them.
    Choice(&'static [(&'static str, &'static str)]),
}

impl Hypr {
    fn on(page: Page, label: &'static str, note: &'static str, name: &'static str, kind: Self) -> Row {
        Row { page, label, note, control: Control::Hypr(name, kind) }
    }
}

struct Row {
    page: Page,
    label: &'static str,
    note: &'static str,
    control: Control,
}

macro_rules! switch {
    ($f:ident) => {
        Control::Switch(|s| s.$f, |s, v| s.$f = v)
    };
}

macro_rules! number {
    ($f:ident, $min:expr, $max:expr, $per:expr, $format:expr) => {
        Control::Number { get: |s| s.$f as f64, set: |s, v| s.$f = v as _, min: $min, max: $max, per: $per, format: $format }
    };
}

macro_rules! text {
    ($f:ident, $placeholder:expr) => {
        Control::Text { get: |s| s.$f.as_str(), set: |s, v| s.$f = v, placeholder: $placeholder }
    };
}

fn px_format(v: f64) -> String {
    format!("{v}px")
}

fn percent(v: f64) -> String {
    format!("{v}%")
}

fn plain(v: f64) -> String {
    v.to_string()
}

fn signed(v: f64) -> String {
    format!("{v:+}")
}

fn rows() -> Vec<Row> {
    use Page::*;
    let row = |page, label, note, control| Row { page, label, note, control };
    vec![
        row(Bar, "System stats", "CPU, memory and temperature.", switch!(bar_stats)),
        row(Bar, "Claude usage", "Claude Code's five-hour rate-limit usage.", switch!(bar_agent_usage)),
        row(Bar, "Network speed", "Download and upload rates.", switch!(bar_net_speed)),
        row(Bar, "Status glyphs", "Volume, brightness, network, Bluetooth and battery.", switch!(bar_status)),
        row(Bar, "Weather", "The current conditions, from the weather location.", switch!(bar_weather)),
        row(Bar, "Tray", "System tray icons.", switch!(bar_tray)),
        row(Bar, "Media", "The playing track.", switch!(bar_media)),
        row(Bar, "Occupied workspaces only", "Hide workspaces past the highest occupied one.", switch!(occupied_workspaces_only)),
        row(Bar, "Workspace icons", "App icons inside occupied workspace pills.", switch!(workspace_icons)),
        row(Bar, "Bar height", "The band's height before scaling.", number!(bar_height, 22.0, 44.0, 1.0, px_format)),
        row(Launcher, "Greeting", "Good morning, afternoon or evening on the card.", switch!(launcher_greeting)),
        row(Launcher, "Weather", "The forecast on the card.", switch!(launcher_weather)),
        row(Launcher, "Corner radius", "Of the launcher card and popouts.", number!(launcher_radius, 0.0, 32.0, 1.0, px_format)),
        row(Launcher, "Background blur", "How hard the desktop blurs behind the launcher; 0 leaves it alone.", number!(launcher_blur, 0.0, 30.0, 1.0, |v| if v == 0.0 { "off".into() } else { v.to_string() })),
        row(Frame, "Edge", "How far the frame shows past the screen edge.", number!(frame_edge, 0.0, 24.0, 1.0, px_format)),
        row(Frame, "Corner radius", "Of the frame's inner corners.", number!(frame_radius, 0.0, 24.0, 1.0, px_format)),
        row(Frame, "Border", "The hairline along the frame's inner edge.", number!(frame_border_width, 0.0, 40.0, 10.0, |v| format!("{}px", v / 10.0))),
        row(Frame, "Shadow size", "How far the frame's shadow falls.", number!(shadow_size, 0.0, 32.0, 1.0, px_format)),
        row(Frame, "Shadow strength", "How dark it is.", number!(shadow_strength, 0.0, 100.0, 100.0, percent)),
        row(Clock, "24-hour clock", "13:00 rather than 1:00 PM.", switch!(clock_24h)),
        row(Clock, "Seconds", "Seconds on the bar's clock.", switch!(clock_seconds)),
        row(Clock, "Weather location", "\"Town, Region\". Leave it empty to locate by IP address.", text!(weather_location, "Located by IP address")),
        row(Fonts, "Scale", "Everything the shell draws, on top of each monitor's scale.", number!(font_scale, 70.0, 160.0, 100.0, percent)),
        row(Fonts, "Font", "For text.", text!(font, "JetBrainsMono Nerd Font")),
        row(Fonts, "Mono font", "For numbers and labels in caps.", text!(mono_font, "JetBrainsMono Nerd Font")),
        row(Fonts, "Icon font", "For glyphs; a Material Symbols font.", text!(icon_font, "Material Symbols Rounded")),
        row(Fonts, "Icon theme", "For app icons.", text!(icon_theme, "Papirus-Dark")),
        Hypr::on(Windows, "Layout", "Dwindle splits the space in two each time; master keeps one big window.", "general:layout", Hypr::Choice(&[("Dwindle", "dwindle"), ("Master", "master")])),
        Hypr::on(Windows, "Inner gaps", "Between windows.", "general:gaps_in", Hypr::Number { min: 0.0, max: 40.0, per: 1.0, format: px_format }),
        Hypr::on(Windows, "Outer gaps", "Between windows and the screen edge.", "general:gaps_out", Hypr::Number { min: 0.0, max: 60.0, per: 1.0, format: px_format }),
        Hypr::on(Windows, "Border", "The coloured line round each window.", "general:border_size", Hypr::Number { min: 0.0, max: 8.0, per: 1.0, format: px_format }),
        Hypr::on(Windows, "Rounding", "Window corner radius.", "decoration:rounding", Hypr::Number { min: 0.0, max: 30.0, per: 1.0, format: px_format }),
        Hypr::on(Windows, "Corner shape", "2 is a circular corner; higher is squarer, like a squircle.", "decoration:rounding_power", Hypr::Number { min: 2.0, max: 10.0, per: 1.0, format: plain }),
        Hypr::on(Windows, "Active opacity", "Of the focused window.", "decoration:active_opacity", Hypr::Number { min: 50.0, max: 100.0, per: 100.0, format: percent }),
        Hypr::on(Windows, "Inactive opacity", "Of every other window.", "decoration:inactive_opacity", Hypr::Number { min: 50.0, max: 100.0, per: 100.0, format: percent }),
        Hypr::on(Windows, "Dim inactive", "Darken windows without focus.", "decoration:dim_inactive", Hypr::Switch),
        Hypr::on(Windows, "Blur", "Blur what shows through translucent windows.", "decoration:blur:enabled", Hypr::Switch),
        Hypr::on(Windows, "Blur size", "How far each pass spreads.", "decoration:blur:size", Hypr::Number { min: 1.0, max: 20.0, per: 1.0, format: plain }),
        Hypr::on(Windows, "Blur passes", "More passes are smoother and cost more.", "decoration:blur:passes", Hypr::Number { min: 1.0, max: 4.0, per: 1.0, format: plain }),
        Hypr::on(Windows, "Shadows", "A shadow under each window.", "decoration:shadow:enabled", Hypr::Switch),
        Hypr::on(Windows, "Shadow range", "How far it falls.", "decoration:shadow:range", Hypr::Number { min: 0.0, max: 100.0, per: 1.0, format: px_format }),
        Hypr::on(Windows, "Resize on border", "Drag a window's edge to resize it.", "general:resize_on_border", Hypr::Switch),
        Hypr::on(Input, "Focus", "Hover: focus follows the pointer. Click: a click moves focus. Loose: the pointer and keyboard focus stay apart.", "input:follow_mouse", Hypr::Choice(&[("Off", "0"), ("Hover", "1"), ("Click", "2"), ("Loose", "3")])),
        Hypr::on(Input, "Pointer speed", "Added to the device's own acceleration.", "input:sensitivity", Hypr::Number { min: -100.0, max: 100.0, per: 100.0, format: signed }),
        Hypr::on(Input, "Natural scrolling (mouse)", "Content moves with your fingers.", "input:natural_scroll", Hypr::Switch),
        Hypr::on(Input, "Natural scrolling (touchpad)", "The same, on the touchpad.", "input:touchpad:natural_scroll", Hypr::Switch),
        Hypr::on(Input, "Tap to click", "A tap on the touchpad clicks.", "input:touchpad:tap_to_click", Hypr::Switch),
        Hypr::on(Input, "Two-finger right click", "Click with two fingers for a right click, instead of the pad's corner.", "input:touchpad:clickfinger_behavior", Hypr::Switch),
        Hypr::on(Input, "Ignore touchpad while typing", "No stray clicks from a palm.", "input:touchpad:disable_while_typing", Hypr::Switch),
        Hypr::on(Input, "Key repeat rate", "Repeats a second while a key is held.", "input:repeat_rate", Hypr::Number { min: 10.0, max: 60.0, per: 1.0, format: |v| format!("{v}/s") }),
        Hypr::on(Input, "Key repeat delay", "How long a key is held before it repeats.", "input:repeat_delay", Hypr::Number { min: 150.0, max: 800.0, per: 1.0, format: |v| format!("{v}ms") }),
    ]
}

pub fn run(args: &[String]) -> glib::ExitCode {
    // `hub windows`, `hub keybinds`: the page whose name starts with the word.
    let start = args.first().and_then(|a| Page::ALL.into_iter().find(|p| p.name().to_lowercase().starts_with(&a.to_lowercase()))).unwrap_or(Page::Bar);
    let app = gtk::Application::builder().application_id(APP_ID).build();
    let shown: Rc<RefCell<Option<Rc<Hub>>>> = Rc::default();
    app.connect_activate(move |app| {
        if let Some(hub) = shown.borrow().as_ref() {
            hub.window.present();
            return;
        }
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(crate::TRANSPARENT_WINDOW);
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        theme::watch_palette();
        theme::font_settings();
        *shown.borrow_mut() = Some(Hub::new(app, start));
    });
    app.run_with_args::<&str>(&[])
}

struct Hub {
    window: gtk::ApplicationWindow,
    s: f64,
    rows: Vec<Row>,
    settings: RefCell<Settings>,
    /// What the running shell started with, as far as the hub knows.
    running: RefCell<Settings>,
    page: Cell<Page>,
    query: RefCell<String>,
    binds: Vec<Group>,
    rail: gtk::Box,
    content: gtk::Box,
    scroller: gtk::ScrolledWindow,
    title: gtk::Label,
    about: gtk::Label,
    footer: gtk::Box,
    restarting: Cell<bool>,
}

impl Hub {
    fn new(app: &gtk::Application, start: Page) -> Rc<Self> {
        let monitor = gdk::Display::default().and_then(|d| d.monitors().item(0)).and_downcast::<gdk::Monitor>();
        let (sw, sh) = monitor.as_ref().map_or((1920, 1080), |m| (m.geometry().width(), m.geometry().height()));
        let s = config::bar_scale(sh);
        let (ww, wh) = ((f64::from(sw) * 0.62).min(1180.0) as i32, (f64::from(sh) * 0.72).min(860.0) as i32);
        hyprland::eval(&format!("hl.window_rule({{ name = \"eclipse-hub\", match = {{ class = \"^({APP_ID})$\" }}, float = true, size = {{ {ww}, {wh} }}, center = true }})"));

        let window = gtk::ApplicationWindow::builder().application(app).title("Hub").default_width(ww).default_height(wh).decorated(false).css_classes(["eclipse-shell"]).build();
        let px = |v: f64| (v * s).round() as i32;

        let back = gtk::DrawingArea::new();
        back.set_draw_func(|_, cr, _, _| {
            theme::set_source(cr, theme::palette().base);
            let _ = cr.paint();
        });
        theme::on_palette_change({
            let back = back.downgrade();
            move || back.upgrade().into_iter().for_each(|b| b.queue_draw())
        });

        let settings = config::load();
        let title = TextStyle::new(15.0 * s, pango::Weight::Bold, BRIGHT).label("");
        title.set_xalign(0.0);
        let about = TextStyle::new(10.5 * s, pango::Weight::Medium, DIM).label("");
        about.set_xalign(0.0);
        about.set_wrap(true);
        let hub = Rc::new(Self {
            window,
            s,
            rows: rows(),
            running: RefCell::new(settings.clone()),
            settings: RefCell::new(settings),
            page: Cell::new(start),
            query: RefCell::default(),
            binds: keybinds::load(),
            rail: gtk::Box::new(gtk::Orientation::Vertical, px(2.0)),
            content: gtk::Box::new(gtk::Orientation::Vertical, px(10.0)),
            scroller: gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build(),
            title,
            about,
            footer: gtk::Box::new(gtk::Orientation::Horizontal, px(12.0)),
            restarting: Cell::new(false),
        });

        // Left: the name, the search, the sections.
        let left = gtk::Box::new(gtk::Orientation::Vertical, px(14.0));
        left.set_size_request(px(RAIL_W), -1);
        // Held at its width; the path label below would otherwise widen it.
        left.set_hexpand(false);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
        head.append(&mark::mark(18.0 * s));
        head.append(&TextStyle::new(15.0 * s, pango::Weight::Bold, BRIGHT).label("Hub"));
        let what = TextStyle::new(10.5 * s, pango::Weight::Medium, DIM).label("Settings");
        what.set_valign(gtk::Align::Baseline);
        head.append(&what);
        left.append(&head);
        let (field, search) = ui::field(s, "search", "Search settings");
        search.connect_changed({
            let hub = Rc::downgrade(&hub);
            move |t| {
                if let Some(hub) = hub.upgrade() {
                    *hub.query.borrow_mut() = t.text().trim().to_lowercase();
                    hub.show();
                }
            }
        });
        left.append(&field);
        left.append(&hub.rail);
        let gap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        gap.set_vexpand(true);
        left.append(&gap);
        let path = TextStyle::new(9.0 * s, pango::Weight::Medium, FAINT).mono().label("~/.config/eclipse-shell/settings.json");
        popout::fill(&path);
        left.append(&path);

        // Right: the section's title, its settings, the footer.
        let right = gtk::Box::new(gtk::Orientation::Vertical, px(12.0));
        right.set_hexpand(true);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
        let titles = gtk::Box::new(gtk::Orientation::Vertical, px(4.0));
        titles.set_hexpand(true);
        titles.append(&hub.title);
        titles.append(&hub.about);
        top.append(&titles);
        let close = app_window::close_button(s, &hub.window);
        close.set_valign(gtk::Align::Start);
        top.append(&close);
        right.append(&top);
        hub.content.set_margin_end(px(12.0));
        hub.scroller.set_child(Some(&hub.content));
        right.append(&hub.scroller);
        right.append(&hub.footer);

        let body = gtk::Box::new(gtk::Orientation::Horizontal, px(18.0));
        body.set_margin_start(px(20.0));
        body.set_margin_end(px(20.0));
        body.set_margin_top(px(18.0));
        body.set_margin_bottom(px(14.0));
        body.append(&left);
        body.append(&ui::rule_v());
        body.append(&right);

        let layers = gtk::Overlay::new();
        layers.set_child(Some(&back));
        layers.add_overlay(&body);
        hub.window.set_child(Some(&layers));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let window = hub.window.downgrade();
            move |_, key, _, modifiers| {
                if key == gdk::Key::Escape || (key == gdk::Key::q && modifiers.contains(gdk::ModifierType::CONTROL_MASK)) {
                    window.upgrade().into_iter().for_each(|w| w.close());
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        hub.window.add_controller(keys);

        hub.show();
        hub.window.present();
        search.grab_focus();
        hub
    }

    fn show(self: &Rc<Self>) {
        self.show_rail();
        self.show_content();
        self.show_footer();
    }

    fn show_rail(self: &Rc<Self>) {
        let s = self.s;
        let px = |v: f64| (v * s).round() as i32;
        while let Some(c) = self.rail.first_child() {
            self.rail.remove(&c);
        }
        let searching = !self.query.borrow().is_empty();
        for page in Page::ALL {
            let on = !searching && page == self.page.get();
            let hovered = Rc::new(Cell::new(false));
            let back = gtk::DrawingArea::new();
            back.set_draw_func({
                let hovered = hovered.clone();
                move |_, cr, w, h| {
                    let palette = theme::palette();
                    let (w, h) = (f64::from(w), f64::from(h));
                    if on || hovered.get() {
                        theme::rounded_rect(cr, 0.0, 0.0, w, h, 8.0 * s);
                        theme::set_source(cr, if on { palette.accent.alpha(0.16) } else { theme::ROW_HOVER });
                        let _ = cr.fill();
                    }
                    if on {
                        theme::rounded_rect(cr, 0.0, h * 0.25, 3.0 * s, h * 0.5, 1.5 * s);
                        theme::set_source(cr, palette.accent_lit);
                        let _ = cr.fill();
                    }
                }
            });
            let row = gtk::Box::new(gtk::Orientation::Horizontal, px(10.0));
            row.set_margin_start(px(12.0));
            row.set_margin_end(px(12.0));
            row.set_margin_top(px(8.0));
            row.set_margin_bottom(px(8.0));
            row.append(&theme::icon_glyph(page.glyph(), 17.0 * s, if on { theme::palette().accent_lit } else { theme::ICON_DIM }));
            row.append(&TextStyle::new(11.0 * s, pango::Weight::Semibold, if on { CREAM } else { SUBTLE }).label(page.name()));
            let item = gtk::Overlay::new();
            item.set_child(Some(&back));
            item.add_overlay(&row);
            item.set_measure_overlay(&row, true);
            popout::on_hover(&item, {
                let back = back.downgrade();
                move |inside| {
                    hovered.set(inside);
                    back.upgrade().into_iter().for_each(|b| b.queue_draw());
                }
            });
            let hub = Rc::downgrade(self);
            popout::on_click(&item, move || {
                if let Some(hub) = hub.upgrade() {
                    hub.page.set(page);
                    hub.show();
                    hub.scroller.vadjustment().set_value(0.0);
                }
            });
            self.rail.append(&item);
        }
    }

    fn show_content(self: &Rc<Self>) {
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        let query = self.query.borrow().clone();
        if query.is_empty() {
            let page = self.page.get();
            self.title.set_text(page.name());
            self.about.set_text(page.about());
            if page == Page::Keybinds {
                self.show_binds(|_| true);
            } else if page == Page::Startup {
                self.show_startup();
            } else {
                self.show_rows(|r| r.page == page);
            }
            return;
        }
        self.title.set_text("Search");
        self.about.set_text(&format!("Settings and keybinds matching \u{201c}{query}\u{201d}."));
        let words: Vec<&str> = query.split_whitespace().collect();
        let matches = |text: String| {
            let text = text.to_lowercase();
            words.iter().all(|w| text.contains(w))
        };
        let mut found = 0;
        for page in Page::ALL.into_iter().filter(|&p| p != Page::Keybinds) {
            let hits = |r: &Row| r.page == page && matches(format!("{} {} {}", page.name(), r.label, r.note));
            if self.rows.iter().any(hits) {
                self.content.append(&popout::eyebrow(self.s, page.name()));
                found += self.show_rows(hits);
            }
        }
        let bind_hits = |b: &keybinds::Bind| matches(format!("keybinds {} {}", b.keys.join(" "), b.what));
        if self.binds.iter().flat_map(|g| &g.binds).any(bind_hits) {
            self.content.append(&popout::eyebrow(self.s, "Keybinds"));
            found += self.show_binds(bind_hits);
        }
        if found == 0 {
            self.content.append(&ui::note(self.s, "search_off", "Nothing matches that."));
        }
    }

    /// Appends a card of the rows `keep` picks; returns how many.
    fn show_rows(self: &Rc<Self>, keep: impl Fn(&Row) -> bool) -> usize {
        let s = self.s;
        let px = |v: f64| (v * s).round() as i32;
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let mut count = 0;
        for (i, row) in self.rows.iter().enumerate().filter(|(_, r)| keep(r)) {
            if count > 0 {
                list.append(&popout::divider());
            }
            count += 1;
            let line = gtk::Box::new(gtk::Orientation::Horizontal, px(16.0));
            line.set_margin_start(px(16.0));
            line.set_margin_end(px(16.0));
            line.set_margin_top(px(11.0));
            line.set_margin_bottom(px(11.0));
            let words = gtk::Box::new(gtk::Orientation::Vertical, px(3.0));
            words.set_hexpand(true);
            words.set_valign(gtk::Align::Center);
            let label = TextStyle::new(11.5 * s, pango::Weight::Semibold, CREAM).label(row.label);
            label.set_xalign(0.0);
            words.append(&label);
            let note = TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).label(row.note);
            note.set_xalign(0.0);
            note.set_wrap(true);
            words.append(&note);
            line.append(&words);
            line.append(&self.control(i));
            list.append(&line);
        }
        if count > 0 {
            self.content.append(&popout::card(&list));
        }
        count
    }

    fn control(self: &Rc<Self>, i: usize) -> gtk::Widget {
        let s = self.s;
        let hub = Rc::downgrade(self);
        match &self.rows[i].control {
            Control::Switch(get, set) => {
                let (on, set) = (get(&self.settings.borrow()), *set);
                popout::switch(s, on, true, move || {
                    if let Some(hub) = hub.upgrade() {
                        hub.change(|settings| set(settings, !on));
                        // Redrawn whole: the switch is drawn at its state.
                        hub.show_content();
                    }
                })
                .upcast()
            }
            Control::Number { get, set, min, max, per, format } => {
                let (value, set, per) = ((get(&self.settings.borrow()) * per).round(), *set, *per);
                let slider = ui::slider(s, "", *min, *max, value, *format, move |v| {
                    if let Some(hub) = hub.upgrade() {
                        hub.change(|settings| set(settings, v / per));
                    }
                });
                slider.set_size_request((CONTROL_W * s).round() as i32, -1);
                slider.set_hexpand(false);
                slider.set_valign(gtk::Align::Center);
                slider.upcast()
            }
            Control::Text { get, set, placeholder } => {
                let (field, text) = ui::field(s, "edit", placeholder);
                text.set_text(get(&self.settings.borrow()));
                let set = *set;
                text.connect_changed(move |t| {
                    if let Some(hub) = hub.upgrade() {
                        hub.change(|settings| set(settings, t.text().trim().to_owned()));
                    }
                });
                field.set_hexpand(false);
                field.set_size_request((CONTROL_W * s).round() as i32, -1);
                field.set_valign(gtk::Align::Center);
                field.upcast()
            }
            Control::Hypr(name, kind) => self.hypr_control(name, kind),
        }
    }

    fn hypr_control(self: &Rc<Self>, name: &'static str, kind: &Hypr) -> gtk::Widget {
        let s = self.s;
        let Some(value) = hypr::live(name) else {
            return TextStyle::new(10.0 * s, pango::Weight::Medium, FAINT).label("Not available").upcast();
        };
        let hub = Rc::downgrade(self);
        match kind {
            Hypr::Switch => {
                let on = value.as_bool().or_else(|| value.as_i64().map(|i| i != 0)).unwrap_or(false);
                popout::switch(s, on, true, move || {
                    if let Some(hub) = hub.upgrade() {
                        hypr::set(name, (!on).into());
                        hub.show_content();
                        hub.show_footer();
                    }
                })
                .upcast()
            }
            Hypr::Number { min, max, per, format } => {
                let (integer, per) = (value.is_i64(), *per);
                let shown = (value.as_f64().unwrap_or(*min / per) * per).round();
                let slider = ui::slider(s, "", *min, *max, shown, *format, move |v| {
                    let v = v / per;
                    hypr::set(name, if integer { (v.round() as i64).into() } else { v.into() });
                    if let Some(hub) = hub.upgrade() {
                        hub.show_footer();
                    }
                });
                slider.set_size_request((CONTROL_W * s).round() as i32, -1);
                slider.set_hexpand(false);
                slider.set_valign(gtk::Align::Center);
                slider.upcast()
            }
            Hypr::Choice(options) => {
                let current = value.as_str().map_or_else(|| value.to_string(), str::to_owned);
                let chosen = options.iter().position(|(_, v)| *v == current).unwrap_or(usize::MAX);
                let labels: Vec<&str> = options.iter().map(|(l, _)| *l).collect();
                let (options, numeric) = (*options, value.is_number());
                let row = ui::segmented(s, &labels, chosen, move |i| {
                    let picked = options[i].1;
                    let v = if numeric { picked.parse::<i64>().map_or_else(|_| picked.into(), Into::into) } else { picked.into() };
                    hypr::set(name, v);
                    if let Some(hub) = hub.upgrade() {
                        hub.show_content();
                        hub.show_footer();
                    }
                });
                row.set_valign(gtk::Align::Center);
                row.upcast()
            }
        }
    }

    fn show_startup(self: &Rc<Self>) {
        let s = self.s;
        let px = |v: f64| (v * s).round() as i32;
        let line = || {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, px(12.0));
            line.set_margin_start(px(16.0));
            line.set_margin_end(px(12.0));
            line.set_margin_top(px(8.0));
            line.set_margin_bottom(px(8.0));
            line
        };
        let command_label = |text: &str, color| {
            let label = TextStyle::new(10.5 * s, pango::Weight::Medium, color).mono().label(text);
            popout::fill(&label);
            label.set_valign(gtk::Align::Center);
            label
        };

        // Added here: removable, runnable, and a field to add another.
        self.content.append(&popout::eyebrow(s, "Added here"));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let commands = hypr::startup();
        for (i, command) in commands.iter().enumerate() {
            let row = line();
            row.append(&command_label(command, CREAM));
            let hub = Rc::downgrade(self);
            let run = command.clone();
            row.append(&icon_button(s, "play_arrow", "Run now", move || {
                hypr::run(&run);
                if let Some(hub) = hub.upgrade() {
                    hub.flash(&format!("Started {run}"));
                }
            }));
            let hub = Rc::downgrade(self);
            row.append(&icon_button(s, "close", "Remove", move || {
                let mut commands = hypr::startup();
                if i < commands.len() {
                    commands.remove(i);
                    hypr::set_startup(&commands);
                }
                if let Some(hub) = hub.upgrade() {
                    hub.show_content();
                }
            }));
            list.append(&row);
            list.append(&popout::divider());
        }
        let add = line();
        let (field, text) = ui::field(s, "add", "A command to start at login, e.g. nm-applet");
        add.append(&field);
        let submit = {
            let (hub, text) = (Rc::downgrade(self), text.downgrade());
            Rc::new(move || {
                let (Some(hub), Some(text)) = (hub.upgrade(), text.upgrade()) else { return };
                let command = text.text().trim().to_owned();
                if command.is_empty() {
                    return;
                }
                let mut commands = hypr::startup();
                if !commands.contains(&command) {
                    commands.push(command);
                    hypr::set_startup(&commands);
                }
                hub.show_content();
            })
        };
        text.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
        add.append(&ui::button(s, Some("add"), "Add", true, true, move || submit()));
        list.append(&add);
        self.content.append(&popout::card(&list));
        let note = TextStyle::new(10.0 * s, pango::Weight::Medium, FAINT).label("Starts at your next login. Run one now with ▶.");
        note.set_xalign(0.0);
        note.set_margin_start(px(4.0));
        self.content.append(&note);

        // From the config files: shown, not edited here.
        let configured = hypr::configured_startup();
        if !configured.is_empty() {
            self.content.append(&popout::eyebrow(s, "From your config"));
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            for (i, (command, file)) in configured.iter().enumerate() {
                if i > 0 {
                    list.append(&popout::divider());
                }
                let row = line();
                row.append(&command_label(command, SUBTLE));
                row.append(&TextStyle::new(9.0 * s, pango::Weight::Medium, FAINT).mono().label(file));
                list.append(&row);
            }
            self.content.append(&popout::card(&list));
        }
    }

    /// Shows `text` in the footer for a moment.
    fn flash(self: &Rc<Self>, text: &str) {
        while let Some(c) = self.footer.first_child() {
            self.footer.remove(&c);
        }
        let label = TextStyle::new(10.0 * self.s, pango::Weight::Medium, SUBTLE).label(text);
        popout::fill(&label);
        self.footer.append(&label);
        let hub = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(3), move || {
            if let Some(hub) = hub.upgrade() {
                hub.show_footer();
            }
        });
    }

    /// Appends a card per keybind group of the binds `keep` picks; returns how many.
    fn show_binds(self: &Rc<Self>, keep: impl Fn(&keybinds::Bind) -> bool) -> usize {
        let s = self.s;
        let px = |v: f64| (v * s).round() as i32;
        if self.binds.is_empty() {
            self.content.append(&ui::note(s, "keyboard", "No binds found in ~/.config/hypr/modules/binds.lua or user.lua."));
            return 0;
        }
        let mut total = 0;
        for group in &self.binds {
            let binds: Vec<&keybinds::Bind> = group.binds.iter().filter(|b| keep(b)).collect();
            if binds.is_empty() {
                continue;
            }
            total += binds.len();
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let heading = popout::micro_label(s, &group.title);
            heading.set_margin_start(px(16.0));
            heading.set_margin_top(px(12.0));
            heading.set_margin_bottom(px(6.0));
            list.append(&heading);
            for bind in binds {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, px(16.0));
                line.set_margin_start(px(16.0));
                line.set_margin_end(px(16.0));
                line.set_margin_top(px(5.0));
                line.set_margin_bottom(px(5.0));
                let caps = gtk::Box::new(gtk::Orientation::Horizontal, px(5.0));
                caps.set_size_request(px(200.0), -1);
                for key in &bind.keys {
                    caps.append(&keycap(s, key));
                }
                line.append(&caps);
                let style = if bind.raw { TextStyle::new(10.0 * s, pango::Weight::Medium, DIM).mono() } else { TextStyle::new(11.0 * s, pango::Weight::Medium, CREAM) };
                let what = style.label(&bind.what);
                what.set_valign(gtk::Align::Center);
                popout::fill(&what);
                line.append(&what);
                list.append(&line);
            }
            list.set_margin_bottom(px(8.0));
            self.content.append(&popout::card(&list));
        }
        total
    }

    /// Applies `edit` and saves, when it changed anything.
    fn change(self: &Rc<Self>, edit: impl FnOnce(&mut Settings)) {
        let mut settings = self.settings.borrow().clone();
        edit(&mut settings);
        if settings == *self.settings.borrow() {
            return;
        }
        if let Err(err) = config::save(&settings) {
            eprintln!("eclipse-shell hub: {}: {err}", config::path().display());
        }
        *self.settings.borrow_mut() = settings;
        self.show_footer();
    }

    fn show_footer(self: &Rc<Self>) {
        let s = self.s;
        while let Some(c) = self.footer.first_child() {
            self.footer.remove(&c);
        }
        if self.query.borrow().is_empty() && self.page.get() == Page::Startup {
            let status = TextStyle::new(10.0 * s, pango::Weight::Medium, FAINT).label("Kept in ~/.config/hypr/eclipse.lua with the hub's other Hyprland changes.");
            popout::fill(&status);
            self.footer.append(&status);
            return;
        }
        if self.query.borrow().is_empty() && matches!(self.page.get(), Page::Windows | Page::Input) {
            let changed = !hypr::overrides().is_empty();
            let status = TextStyle::new(10.0 * s, pango::Weight::Medium, FAINT).label("Changes apply at once and are kept in ~/.config/hypr/eclipse.lua.");
            popout::fill(&status);
            status.set_valign(gtk::Align::Center);
            self.footer.append(&status);
            let hub = Rc::downgrade(self);
            self.footer.append(&ui::button(s, Some("undo"), "Undo hub changes", false, changed, move || {
                hypr::reset();
                // Read back once the reload has settled.
                let hub = hub.clone();
                glib::timeout_add_local_once(Duration::from_millis(300), move || {
                    if let Some(hub) = hub.upgrade() {
                        hub.show_content();
                        hub.show_footer();
                    }
                });
            }));
            return;
        }
        let pending = *self.settings.borrow() != *self.running.borrow();
        let text = if self.restarting.get() {
            "Restarting the shell…"
        } else if pending {
            "Saved. The shell picks changes up when it restarts."
        } else {
            "Changes save as you make them."
        };
        let status = TextStyle::new(10.0 * s, pango::Weight::Medium, if pending { SUBTLE } else { FAINT }).label(text);
        popout::fill(&status);
        status.set_valign(gtk::Align::Center);
        self.footer.append(&status);
        let defaults = *self.settings.borrow() != Settings::default();
        let hub = Rc::downgrade(self);
        self.footer.append(&ui::button(s, Some("restart_alt"), "Defaults", false, defaults, move || {
            if let Some(hub) = hub.upgrade() {
                hub.change(|settings| *settings = Settings::default());
                hub.show_content();
            }
        }));
        let hub = Rc::downgrade(self);
        self.footer.append(&ui::button(s, Some("refresh"), "Restart shell", pending, pending && !self.restarting.get(), move || {
            if let Some(hub) = hub.upgrade() {
                hub.restart_shell();
            }
        }));
    }

    /// Asks the running shell to quit, then starts a fresh one on the saved settings.
    fn restart_shell(self: &Rc<Self>) {
        let Ok(exe) = std::env::current_exe() else { return };
        self.restarting.set(true);
        self.show_footer();
        let (done, finished) = async_channel::bounded::<()>(1);
        std::thread::spawn(move || {
            let _ = std::process::Command::new(&exe).arg("quit").status();
            // Until the old one has let go of its D-Bus name, a new one would
            // only hand it the command line.
            std::thread::sleep(Duration::from_millis(600));
            use std::os::unix::process::CommandExt;
            let _ = std::process::Command::new(&exe).process_group(0).stdin(std::process::Stdio::null()).spawn();
            let _ = done.send_blocking(());
        });
        let hub = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let _ = finished.recv().await;
            if let Some(hub) = hub.upgrade() {
                hub.restarting.set(false);
                *hub.running.borrow_mut() = hub.settings.borrow().clone();
                hub.show_footer();
            }
        });
    }
}

/// A key on a raised, rounded cap.
fn keycap(s: f64, key: &str) -> gtk::Overlay {
    let px = |v: f64| (v * s).round() as i32;
    let text = TextStyle::new(9.5 * s, pango::Weight::Semibold, CREAM).mono().label(key);
    text.set_margin_start(px(8.0));
    text.set_margin_end(px(8.0));
    text.set_margin_top(px(3.0));
    text.set_margin_bottom(px(4.0));
    let back = gtk::DrawingArea::new();
    back.set_draw_func(move |_, cr, w, h| {
        let palette = theme::palette();
        let (w, h) = (f64::from(w), f64::from(h));
        theme::rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, 6.0 * s);
        theme::set_source(cr, palette.raised);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        theme::set_source(cr, palette.outline);
        let _ = cr.stroke();
        // A heavier lower lip, as on a real key.
        cr.rectangle(6.0 * s, h - 1.5, w - 12.0 * s, 1.0);
        theme::set_source(cr, theme::HAIR);
        let _ = cr.fill();
    });
    let cap = gtk::Overlay::new();
    cap.set_child(Some(&back));
    cap.add_overlay(&text);
    cap.set_measure_overlay(&text, true);
    cap.set_valign(gtk::Align::Center);
    cap
}

/// A glyph that lights on hover and does `pressed` on a click; `hint` names it.
fn icon_button(s: f64, glyph: &str, hint: &str, pressed: impl Fn() + 'static) -> gtk::Label {
    let icon = theme::icon_glyph(glyph, 17.0 * s, theme::ICON_DIM);
    icon.set_valign(gtk::Align::Center);
    icon.set_accessible_role(gtk::AccessibleRole::Button);
    icon.update_property(&[gtk::accessible::Property::Label(hint)]);
    popout::on_hover(&icon, {
        let (icon, glyph) = (icon.downgrade(), glyph.to_owned());
        move |inside| {
            if let Some(icon) = icon.upgrade() {
                let color = if inside { theme::palette().accent_lit } else { theme::ICON_DIM };
                icon.set_attributes(theme::icon_glyph(&glyph, 17.0 * s, color).attributes().as_ref());
            }
        }
    });
    popout::on_click(&icon, pressed);
    icon
}
