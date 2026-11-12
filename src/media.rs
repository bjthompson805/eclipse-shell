//! Media players over MPRIS: follows every `org.mpris.MediaPlayer2.*` name on
//! the session bus and picks the one to show, the first playing player or
//! else the first one present. A live wallpaper (mpvpaper) registers too;
//! a bare video file name for a title marks it as scenery, not music.

use std::cell::RefCell;

use gtk::glib::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Clone, Default, PartialEq)]
pub struct Player {
    pub name: String,
    pub playing: bool,
    pub title: String,
    pub artists: String,
    pub art_url: String,
}

impl Player {
    /// "Title · Artist", or just the title.
    pub fn line(&self) -> String {
        if self.artists.is_empty() { self.title.clone() } else { format!("{} \u{b7} {}", self.title, self.artists) }
    }

    fn is_wallpaper(&self) -> bool {
        let t = self.title.to_lowercase();
        [".mp4", ".webm", ".mkv", ".gif"].iter().any(|ext| t.ends_with(ext))
    }
}

thread_local! {
    static CONNECTION: RefCell<Option<gio::DBusConnection>> = RefCell::default();
    static PLAYERS: RefCell<Vec<Player>> = RefCell::default();
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = RefCell::default();
    static SUBSCRIPTIONS: RefCell<Vec<(String, gio::SignalSubscription)>> = RefCell::default();
}

/// The player to show, if any has a track.
pub fn current() -> Option<Player> {
    PLAYERS.with(|p| {
        let p = p.borrow();
        let real: Vec<&Player> = p.iter().filter(|p| !p.is_wallpaper()).collect();
        real.iter().find(|p| p.playing).or(real.first()).map(|p| (*p).clone())
    })
    .filter(|p| !p.title.is_empty())
}

pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

fn changed() {
    LISTENERS.with(|l| l.borrow().iter().for_each(|f| f()));
}

pub fn start() {
    glib::spawn_future_local(async {
        let Ok(connection) = gio::bus_get_future(gio::BusType::Session).await else { return };
        CONNECTION.with(|c| *c.borrow_mut() = Some(connection.clone()));

        // Players coming and going.
        let owners = connection.subscribe_to_signal(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            None,
            gio::DBusSignalFlags::NONE,
            |signal| {
                let Some((name, _, new_owner)) = signal.parameters.get::<(String, String, String)>() else { return };
                if !name.starts_with(PREFIX) {
                    return;
                }
                if new_owner.is_empty() { remove(&name) } else { add(&name) }
            },
        );
        std::mem::forget(owners);

        let reply = connection
            .call_future(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ListNames",
                None,
                Some(glib::VariantTy::new("(as)").unwrap()),
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await;
        if let Some((names,)) = reply.ok().and_then(|r| r.get::<(Vec<String>,)>()) {
            names.iter().filter(|n| n.starts_with(PREFIX)).for_each(|n| add(n));
        }
    });
}

fn connection() -> Option<gio::DBusConnection> {
    CONNECTION.with(|c| c.borrow().clone())
}

fn add(name: &str) {
    let Some(connection) = connection() else { return };
    if PLAYERS.with(|p| p.borrow().iter().any(|p| p.name == name)) {
        return;
    }
    PLAYERS.with(|p| p.borrow_mut().push(Player { name: name.to_owned(), ..Player::default() }));
    let subscription = connection.subscribe_to_signal(
        Some(name),
        Some("org.freedesktop.DBus.Properties"),
        Some("PropertiesChanged"),
        Some(PATH),
        None,
        gio::DBusSignalFlags::NONE,
        {
            let name = name.to_owned();
            move |_| refresh(&name)
        },
    );
    SUBSCRIPTIONS.with(|s| s.borrow_mut().push((name.to_owned(), subscription)));
    refresh(name);
}

fn remove(name: &str) {
    PLAYERS.with(|p| p.borrow_mut().retain(|p| p.name != name));
    SUBSCRIPTIONS.with(|s| s.borrow_mut().retain(|(n, _)| n != name));
    changed();
}

fn refresh(name: &str) {
    let Some(connection) = connection() else { return };
    let name = name.to_owned();
    glib::spawn_future_local(async move {
        let reply = connection
            .call_future(
                Some(&name),
                PATH,
                "org.freedesktop.DBus.Properties",
                "GetAll",
                Some(&(PLAYER_IFACE,).to_variant()),
                Some(glib::VariantTy::new("(a{sv})").unwrap()),
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await;
        let Ok(reply) = reply else { return };
        let props = glib::VariantDict::new(Some(&reply.child_value(0)));
        let status = props.lookup::<String>("PlaybackStatus").ok().flatten().unwrap_or_default();
        let meta = props.lookup_value("Metadata", None).map(|m| glib::VariantDict::new(Some(&m)));
        let text = |k: &str| meta.as_ref().and_then(|m| m.lookup::<String>(k).ok().flatten()).unwrap_or_default();
        let artists = meta
            .as_ref()
            .and_then(|m| m.lookup::<Vec<String>>("xesam:artist").ok().flatten())
            .map(|a| a.join(", "))
            .unwrap_or_default();
        let updated = Player { name: name.clone(), playing: status == "Playing", title: text("xesam:title"), artists, art_url: text("mpris:artUrl") };
        let differs = PLAYERS.with(|p| {
            let mut p = p.borrow_mut();
            match p.iter_mut().find(|p| p.name == name) {
                Some(slot) if *slot != updated => {
                    *slot = updated;
                    true
                }
                _ => false,
            }
        });
        if differs {
            changed();
        }
    });
}

pub fn play_pause(player: &Player) {
    let Some(connection) = connection() else { return };
    connection.call(Some(&player.name), PATH, PLAYER_IFACE, "PlayPause", None, None, gio::DBusCallFlags::NONE, 5000, gio::Cancellable::NONE, |_| {});
}
