mod agent_usage;
mod anim;
mod bar;
mod battery_popout;
mod calendar;
mod config;
mod hyprland;
mod icons;
mod launcher;
mod mark;
mod media;
mod popout;
mod power;
mod theme;
mod tray;
mod usage_popout;
mod weather;
mod window_popout;

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;

use bar::{Actions, Bar};
use battery_popout::BatteryPopout;
use calendar::Calendar;
use hyprland::State;
use launcher::Launcher;
use power::Power;
use usage_popout::UsagePopout;
use window_popout::WindowPopout;

/// GTK windows paint a theme background by default; this is the one rule
/// needed to let the drawn surfaces show through.
const TRANSPARENT_WINDOW: &str = "window.eclipse-shell { background: transparent; }";

struct Shell {
    bars: Vec<Bar>,
    state: State,
    launcher: Rc<Launcher>,
    power: Rc<Power>,
    calendar: Rc<Calendar>,
    usage: Rc<UsagePopout>,
    battery: Rc<BatteryPopout>,
    window: Rc<WindowPopout>,
}

impl Shell {
    /// The bar on the focused monitor, or any bar.
    fn focused_bar(&self) -> Option<&Bar> {
        let focused = self.state.focused_monitor.as_deref();
        self.bars.iter().find(|b| b.connector.as_deref() == focused).or(self.bars.first())
    }

    fn refresh(&self, display: &gdk::Display) {
        for bar in &self.bars {
            bar.update(&self.state);
        }
        let focused = monitor_named(display, self.state.focused_monitor.as_deref());
        self.launcher.set_monitor(focused.clone());
        self.power.set_monitor(focused.clone());
        self.calendar.set_monitor(focused.clone());
        self.usage.set_monitor(focused.clone());
        self.battery.set_monitor(focused.clone());
        self.window.set_monitor(focused);
    }
}

fn monitor_named(display: &gdk::Display, connector: Option<&str>) -> Option<gdk::Monitor> {
    let monitors = display.monitors();
    let all = (0..monitors.n_items()).filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>());
    let mut first = None;
    for monitor in all {
        if connector.is_some() && monitor.connector().as_deref() == connector {
            return Some(monitor);
        }
        first.get_or_insert(monitor);
    }
    first
}

fn rebuild_bars(app: &gtk::Application, display: &gdk::Display, shell: &Rc<RefCell<Shell>>) {
    let mut shell_ref = shell.borrow_mut();
    for bar in shell_ref.bars.drain(..) {
        bar.destroy();
    }
    let (launcher, power, calendar, usage) = (shell_ref.launcher.clone(), shell_ref.power.clone(), shell_ref.calendar.clone(), shell_ref.usage.clone());
    let (battery, window) = (shell_ref.battery.clone(), shell_ref.window.clone());
    let actions = Rc::new(Actions {
        launcher: Rc::new(move || launcher.toggle()),
        power: Rc::new(move || power.toggle()),
        calendar: Rc::new(move |x| calendar.toggle(x)),
        usage: Rc::new(move |x| usage.toggle(x)),
        battery: Rc::new(move |x| battery.toggle(x)),
        window: Rc::new(move |x| window.toggle(x)),
    });
    let monitors = display.monitors();
    shell_ref.bars = (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
        .map(|monitor| Bar::new(app, &monitor, &actions))
        .collect();
    shell_ref.refresh(display);
}

/// The shell's on-demand surfaces, reachable from the command line.
struct Surfaces {
    launcher: Rc<Launcher>,
    power: Rc<Power>,
    calendar: Rc<Calendar>,
    usage: Rc<UsagePopout>,
    battery: Rc<BatteryPopout>,
    window: Rc<WindowPopout>,
    shell: Rc<RefCell<Shell>>,
}

fn start(app: &gtk::Application) -> Option<Surfaces> {
    let Some(display) = gdk::Display::default() else {
        eprintln!("eclipse-shell: no display");
        return None;
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_string(TRANSPARENT_WINDOW);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

    // Bars come and go with monitors; keep running even with none attached.
    std::mem::forget(app.hold());

    icons::use_desktop_theme();
    theme::watch_palette();
    weather::start();
    tray::start();
    media::start();
    agent_usage::start();

    let launcher = Launcher::new(app);
    let power = Power::new(app);
    let calendar = Calendar::new(app);
    let usage = UsagePopout::new(app);
    let battery = BatteryPopout::new(app);
    let window = WindowPopout::new(app);
    let shell = Rc::new(RefCell::new(Shell { bars: Vec::new(), state: State::default(), launcher: launcher.clone(), power: power.clone(), calendar: calendar.clone(), usage: usage.clone(), battery: battery.clone(), window: window.clone() }));
    rebuild_bars(app, &display, &shell);

    display.monitors().connect_items_changed({
        let (app, display, shell) = (app.clone(), display.clone(), shell.clone());
        move |_, _, _, _| rebuild_bars(&app, &display, &shell)
    });

    let events = hyprland::watch();
    glib::spawn_future_local({
        let shell = shell.clone();
        async move {
            while let Ok(state) = events.recv().await {
                let mut shell = shell.borrow_mut();
                shell.state = state;
                shell.refresh(&display);
            }
        }
    });

    Some(Surfaces { launcher, power, calendar, usage, battery, window, shell })
}

/// The first instance runs the shell. Later invocations, e.g.
/// `eclipse-shell toggle-launcher` from a keybinding, are forwarded to it over
/// D-Bus by GApplication and exit straight away.
fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("io.github.bjthompson805.EclipseShell")
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let surfaces: Rc<OnceCell<Surfaces>> = Rc::default();
    app.connect_activate({
        let surfaces = surfaces.clone();
        move |app| {
            if surfaces.get().is_none() {
                if let Some(started) = start(app) {
                    let _ = surfaces.set(started);
                }
            }
        }
    });
    app.connect_command_line(move |app, command_line| {
        app.activate();
        let args = command_line.arguments();
        match args.get(1).and_then(|arg| arg.to_str()) {
            None => {}
            Some("toggle-launcher") => {
                if let Some(surfaces) = surfaces.get() {
                    surfaces.launcher.toggle();
                }
            }
            Some("toggle-calendar") => {
                if let Some(surfaces) = surfaces.get() {
                    // Under the focused monitor's clock, as a click on it would.
                    let shell = surfaces.shell.borrow();
                    let bar = shell.focused_bar();
                    match bar.and_then(Bar::clock_x) {
                        Some(x) => surfaces.calendar.toggle(x),
                        None => surfaces.calendar.toggle_centred(),
                    }
                }
            }
            Some("toggle-usage") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    let bar = shell.focused_bar();
                    if let Some(x) = bar.and_then(Bar::usage_x) {
                        surfaces.usage.toggle(x);
                    }
                }
            }
            Some("toggle-battery") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    let bar = shell.focused_bar();
                    if let Some(x) = bar.and_then(Bar::battery_x) {
                        surfaces.battery.toggle(x);
                    }
                }
            }
            Some("toggle-window") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    let bar = shell.focused_bar();
                    if let Some(x) = bar.and_then(Bar::title_x) {
                        surfaces.window.toggle(x);
                    }
                }
            }
            Some("toggle-power") => {
                if let Some(surfaces) = surfaces.get() {
                    surfaces.power.toggle();
                }
            }
            Some(other) => {
                command_line.printerr_literal(&format!("eclipse-shell: unknown command '{other}'\n"));
                return glib::ExitCode::FAILURE;
            }
        }
        glib::ExitCode::SUCCESS
    });
    app.run()
}
