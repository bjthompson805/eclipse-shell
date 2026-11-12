mod agent_usage;
mod anim;
mod audio;
mod bar;
mod battery_popout;
mod bluetooth;
mod bluetooth_popout;
mod calendar;
mod capture;
mod config;
mod display_popout;
mod fader;
mod hint;
mod hyprland;
mod icons;
mod inbox_popout;
mod launcher;
mod mark;
mod media;
mod media_popout;
mod network_popout;
mod notifications;
mod mixer_popout;
mod overview;
mod popout;
mod power;
mod resources_popout;
mod sysstats;
mod theme;
mod traffic;
mod traffic_popout;
mod toasts;
mod tray;
mod usage_popout;
mod wallpaper;
mod weather;
mod weather_popout;
mod wifi;
mod window_popout;
mod workspaces_popout;

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;

use bar::{Actions, Bar};
use battery_popout::BatteryPopout;
use bluetooth_popout::BluetoothPopout;
use calendar::Calendar;
use display_popout::DisplayPopout;
use hyprland::State;
use inbox_popout::InboxPopout;
use launcher::Launcher;
use media_popout::MediaPopout;
use mixer_popout::MixerPopout;
use network_popout::NetworkPopout;
use overview::Overview;
use power::Power;
use resources_popout::ResourcesPopout;
use toasts::Toasts;
use traffic_popout::TrafficPopout;
use usage_popout::UsagePopout;
use weather_popout::WeatherPopout;
use window_popout::WindowPopout;
use workspaces_popout::WorkspacesPopout;

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
    mixer: Rc<MixerPopout>,
    display: Rc<DisplayPopout>,
    network: Rc<NetworkPopout>,
    bluetooth: Rc<BluetoothPopout>,
    media: Rc<MediaPopout>,
    weather: Rc<WeatherPopout>,
    traffic: Rc<TrafficPopout>,
    workspaces: Rc<WorkspacesPopout>,
    overview: Rc<Overview>,
    toasts: Rc<Toasts>,
    inbox: Rc<InboxPopout>,
    battery: Rc<BatteryPopout>,
    window: Rc<WindowPopout>,
    resources: Rc<ResourcesPopout>,
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
        self.mixer.set_monitor(focused.clone());
        self.display.set_monitor(focused.clone());
        self.network.set_monitor(focused.clone());
        self.bluetooth.set_monitor(focused.clone());
        self.media.set_monitor(focused.clone());
        self.weather.set_monitor(focused.clone());
        self.traffic.set_monitor(focused.clone());
        self.workspaces.set_monitor(focused.clone());
        self.overview.set_monitor(focused.clone());
        self.toasts.set_monitor(focused.clone());
        self.inbox.set_monitor(focused.clone());
        self.battery.set_monitor(focused.clone());
        self.window.set_monitor(focused.clone());
        self.resources.set_monitor(focused);
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
    let (mixer, display_popout) = (shell_ref.mixer.clone(), shell_ref.display.clone());
    let (network, bluetooth, media) = (shell_ref.network.clone(), shell_ref.bluetooth.clone(), shell_ref.media.clone());
    let (weather, traffic, spaces) = (shell_ref.weather.clone(), shell_ref.traffic.clone(), shell_ref.workspaces.clone());
    let inbox = shell_ref.inbox.clone();
    let (battery, window, resources) = (shell_ref.battery.clone(), shell_ref.window.clone(), shell_ref.resources.clone());
    let actions = Rc::new(Actions {
        launcher: Rc::new(move || launcher.toggle()),
        power: Rc::new(move || power.toggle()),
        calendar: Rc::new(move |x| calendar.toggle(x)),
        usage: Rc::new(move |x| usage.toggle(x)),
        mixer: Rc::new(move |x| mixer.toggle(x)),
        display: Rc::new(move |x| display_popout.toggle(x)),
        network: Rc::new(move |x| network.toggle(x)),
        bluetooth: Rc::new(move |x| bluetooth.toggle(x)),
        media: Rc::new(move |x| media.toggle(x)),
        weather: Rc::new(move |x| weather.toggle(x)),
        traffic: Rc::new(move |x| traffic.toggle(x)),
        workspaces: Rc::new(move |x| spaces.toggle(x)),
        inbox: Rc::new(move |x| inbox.toggle(x)),
        battery: Rc::new(move |x| battery.toggle(x)),
        window: Rc::new(move |x| window.toggle(x)),
        resources: Rc::new(move |x| resources.toggle(x)),
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
    mixer: Rc<MixerPopout>,
    display: Rc<DisplayPopout>,
    network: Rc<NetworkPopout>,
    bluetooth: Rc<BluetoothPopout>,
    media: Rc<MediaPopout>,
    weather: Rc<WeatherPopout>,
    traffic: Rc<TrafficPopout>,
    workspaces: Rc<WorkspacesPopout>,
    overview: Rc<Overview>,
    inbox: Rc<InboxPopout>,
    battery: Rc<BatteryPopout>,
    window: Rc<WindowPopout>,
    resources: Rc<ResourcesPopout>,
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
    notifications::start();
    media::start();
    agent_usage::start();
    sysstats::start();
    capture::start();
    traffic::start();
    display_popout::restore();
    bluetooth::start();

    let launcher = Launcher::new(app);
    let power = Power::new(app);
    let calendar = Calendar::new(app);
    let usage = UsagePopout::new(app);
    let mixer = MixerPopout::new(app);
    let display_popout = DisplayPopout::new(app);
    let network = NetworkPopout::new(app);
    let bluetooth = BluetoothPopout::new(app);
    let media_popout = MediaPopout::new(app);
    let weather_popout = WeatherPopout::new(app);
    let traffic_popout = TrafficPopout::new(app);
    let workspaces_popout = WorkspacesPopout::new(app);
    let overview = Overview::new(app);
    let toasts = Toasts::new(app);
    let inbox = InboxPopout::new(app);
    let battery = BatteryPopout::new(app);
    let window = WindowPopout::new(app);
    let resources = ResourcesPopout::new(app);
    let shell = Rc::new(RefCell::new(Shell { bars: Vec::new(), state: State::default(), launcher: launcher.clone(), power: power.clone(), calendar: calendar.clone(), usage: usage.clone(), mixer: mixer.clone(), display: display_popout.clone(), network: network.clone(), bluetooth: bluetooth.clone(), media: media_popout.clone(), weather: weather_popout.clone(), traffic: traffic_popout.clone(), workspaces: workspaces_popout.clone(), overview: overview.clone(), toasts, inbox: inbox.clone(), battery: battery.clone(), window: window.clone(), resources: resources.clone() }));
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

    Some(Surfaces { launcher, power, calendar, usage, mixer, display: display_popout, network, bluetooth, media: media_popout, weather: weather_popout, traffic: traffic_popout, workspaces: workspaces_popout, overview, inbox, battery, window, resources, shell })
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
            Some("toggle-mixer") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::volume_x) {
                        surfaces.mixer.toggle(x);
                    }
                }
            }
            Some("toggle-display") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::display_x) {
                        surfaces.display.toggle(x);
                    }
                }
            }
            Some("toggle-network") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::network_x) {
                        surfaces.network.toggle(x);
                    }
                }
            }
            Some("toggle-bluetooth") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::bluetooth_x) {
                        surfaces.bluetooth.toggle(x);
                    }
                }
            }
            Some("toggle-media") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::media_x) {
                        surfaces.media.toggle(x);
                    }
                }
            }
            Some("toggle-weather") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::weather_x) {
                        surfaces.weather.toggle(x);
                    }
                }
            }
            Some("toggle-traffic") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::traffic_x) {
                        surfaces.traffic.toggle(x);
                    }
                }
            }
            Some("toggle-workspaces") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::workspaces_x) {
                        surfaces.workspaces.toggle(x);
                    }
                }
            }
            Some("toggle-overview") => {
                if let Some(surfaces) = surfaces.get() {
                    surfaces.overview.toggle();
                }
            }
            Some("toggle-inbox") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::bell_x) {
                        surfaces.inbox.toggle(x);
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
            Some("toggle-resources") => {
                if let Some(surfaces) = surfaces.get() {
                    let shell = surfaces.shell.borrow();
                    if let Some(x) = shell.focused_bar().and_then(Bar::stats_x) {
                        surfaces.resources.toggle(x);
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
