mod anim;
mod bar;
mod config;
mod hyprland;
mod icons;
mod launcher;
mod mark;
mod theme;
mod weather;

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;

use bar::Bar;
use hyprland::State;
use launcher::Launcher;

/// GTK windows paint a theme background by default; this is the one rule
/// needed to let the drawn surfaces show through.
const TRANSPARENT_WINDOW: &str = "window.eclipse-shell { background: transparent; }";

struct Shell {
    bars: Vec<Bar>,
    state: State,
    launcher: Rc<Launcher>,
}

impl Shell {
    fn refresh(&self, display: &gdk::Display) {
        for bar in &self.bars {
            bar.update(&self.state);
        }
        self.launcher.set_monitor(monitor_named(display, self.state.focused_monitor.as_deref()));
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
    let launcher = shell_ref.launcher.clone();
    let toggle: Rc<dyn Fn()> = Rc::new(move || launcher.toggle());
    let monitors = display.monitors();
    shell_ref.bars = (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
        .map(|monitor| Bar::new(app, &monitor, toggle.clone()))
        .collect();
    shell_ref.refresh(display);
}

fn start(app: &gtk::Application) -> Option<Rc<Launcher>> {
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

    let launcher = Launcher::new(app);
    let shell = Rc::new(RefCell::new(Shell { bars: Vec::new(), state: State::default(), launcher: launcher.clone() }));
    rebuild_bars(app, &display, &shell);

    display.monitors().connect_items_changed({
        let (app, display, shell) = (app.clone(), display.clone(), shell.clone());
        move |_, _, _, _| rebuild_bars(&app, &display, &shell)
    });

    let events = hyprland::watch();
    glib::spawn_future_local(async move {
        while let Ok(state) = events.recv().await {
            let mut shell = shell.borrow_mut();
            shell.state = state;
            shell.refresh(&display);
        }
    });

    Some(launcher)
}

/// The first instance runs the shell. Later invocations, e.g.
/// `eclipse-shell toggle-launcher` from a keybinding, are forwarded to it over
/// D-Bus by GApplication and exit straight away.
fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("io.github.bjthompson805.EclipseShell")
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let launcher: Rc<OnceCell<Rc<Launcher>>> = Rc::default();
    app.connect_activate({
        let launcher = launcher.clone();
        move |app| {
            if launcher.get().is_none() {
                if let Some(started) = start(app) {
                    let _ = launcher.set(started);
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
                if let Some(launcher) = launcher.get() {
                    launcher.toggle();
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
