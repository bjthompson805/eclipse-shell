//! Notifications: the shell is the session's notification server
//! (org.freedesktop.Notifications). It keeps every notification for the
//! session until it's dismissed, so the inbox can show them, and tells the
//! toasts and the bell about arrivals and changes.
//!
//! Bodies arrive as plain text with, from some apps, a little markup despite
//! the server not asking for it; tags are dropped and entities decoded.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;

use crate::hyprland;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const XML: &str = r#"
<node>
  <interface name="org.freedesktop.Notifications">
    <method name="GetCapabilities"><arg type="as" direction="out"/></method>
    <method name="Notify">
      <arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/>
      <arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/>
      <arg type="a{sv}" direction="in"/><arg type="i" direction="in"/>
      <arg type="u" direction="out"/>
    </method>
    <method name="CloseNotification"><arg type="u" direction="in"/></method>
    <method name="GetServerInformation">
      <arg type="s" direction="out"/><arg type="s" direction="out"/><arg type="s" direction="out"/><arg type="s" direction="out"/>
    </method>
    <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
    <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
  </interface>
</node>"#;

/// Why a notification closed, as the spec numbers them.
#[derive(Clone, Copy)]
enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

#[derive(Clone)]
pub enum Picture {
    Texture(gdk::Texture),
    Icon(gio::Icon),
}

#[derive(Clone)]
pub struct Notification {
    pub id: u32,
    pub app: String,
    pub summary: String,
    pub body: String,
    /// Buttons, key then label; "default" is the click on the notification
    /// itself and isn't shown as a button.
    pub actions: Vec<(String, String)>,
    pub urgency: Urgency,
    pub picture: Option<Picture>,
    /// The desktop entry, for finding the app's window.
    pub desktop_entry: String,
    pub arrived: glib::DateTime,
    /// Milliseconds the app wants it shown: None for the server's choice,
    /// Some(0) for never expiring.
    pub timeout: Option<u32>,
    /// Gone from the inbox once its toast expires.
    pub transient: bool,
    /// Stays after an action is invoked.
    pub resident: bool,
    pub read: bool,
}

impl Notification {
    pub fn has_default(&self) -> bool {
        self.actions.iter().any(|(key, _)| key == "default")
    }

    /// The actions worth a button: everything but "default" with a label.
    pub fn buttons(&self) -> impl Iterator<Item = &(String, String)> {
        self.actions.iter().filter(|(key, label)| key != "default" && !label.trim().is_empty())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Change {
    Added(u32),
    Replaced(u32),
    Removed(u32),
    /// Read state or Do Not Disturb changed.
    State,
}

thread_local! {
    static CONNECTION: RefCell<Option<gio::DBusConnection>> = RefCell::default();
    /// Oldest first.
    static LIST: RefCell<Vec<Notification>> = RefCell::default();
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
    static DND: Cell<bool> = const { Cell::new(false) };
    static LISTENERS: RefCell<Vec<Rc<dyn Fn(Change)>>> = RefCell::default();
}

pub fn on_change(f: impl Fn(Change) + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Rc::new(f)));
}

fn changed(change: Change) {
    // Listeners may read the list or change it; call them with nothing borrowed.
    let listeners: Vec<_> = LISTENERS.with(|l| l.borrow().clone());
    listeners.iter().for_each(|f| f(change));
}

/// Every notification, oldest first.
pub fn all() -> Vec<Notification> {
    LIST.with(|l| l.borrow().clone())
}

pub fn get(id: u32) -> Option<Notification> {
    LIST.with(|l| l.borrow().iter().find(|n| n.id == id).cloned())
}

pub fn unread() -> usize {
    LIST.with(|l| l.borrow().iter().filter(|n| !n.read).count())
}

pub fn mark_all_read() {
    let any = LIST.with(|l| {
        let mut l = l.borrow_mut();
        let any = l.iter().any(|n| !n.read);
        l.iter_mut().for_each(|n| n.read = true);
        any
    });
    if any {
        changed(Change::State);
    }
}

pub fn dnd() -> bool {
    DND.with(Cell::get)
}

pub fn set_dnd(on: bool) {
    if DND.with(|d| d.replace(on)) != on {
        changed(Change::State);
    }
}

/// The user dismissed it.
pub fn dismiss(id: u32) {
    close(id, Reason::Dismissed);
}

/// Dismisses everything.
pub fn clear() {
    for n in all() {
        close(n.id, Reason::Dismissed);
    }
}

/// Its toast ran out: a transient one goes; the rest stay in the inbox.
pub fn expired(id: u32) {
    if get(id).is_some_and(|n| n.transient) {
        close(id, Reason::Expired);
    }
}

/// Runs one of its actions, then dismisses it unless it's resident.
pub fn invoke(id: u32, key: &str) {
    let Some(n) = get(id) else { return };
    emit("ActionInvoked", (id, key).to_variant());
    if !n.resident {
        close(id, Reason::Dismissed);
    }
}

/// A click on the notification: its default action if it has one, then its
/// app's window brought forward, then it's dismissed.
pub fn activate(id: u32) {
    let Some(n) = get(id) else { return };
    if n.has_default() {
        emit("ActionInvoked", (id, "default").to_variant());
    }
    focus_app(&n);
    if !n.resident || !n.has_default() {
        close(id, Reason::Dismissed);
    }
}

/// Focuses a window of the app that sent `n`, if one is open.
fn focus_app(n: &Notification) {
    let names: Vec<String> = [&n.desktop_entry, &n.app].iter().map(|s| s.to_lowercase()).filter(|s| !s.is_empty()).collect();
    if names.is_empty() {
        return;
    }
    glib::spawn_future_local(async move {
        let Ok(Ok(layout)) = gio::spawn_blocking(hyprland::layout).await else { return };
        let found = layout.windows.iter().find(|w| {
            let class = w.class.to_lowercase();
            names.iter().any(|name| class == *name || class.contains(name.as_str()) || name.contains(class.as_str()))
        });
        if let Some(window) = found {
            hyprland::focus_window(&window.address);
        }
    });
}

fn close(id: u32, reason: Reason) {
    let removed = LIST.with(|l| {
        let mut l = l.borrow_mut();
        let before = l.len();
        l.retain(|n| n.id != id);
        l.len() != before
    });
    if removed {
        emit("NotificationClosed", (id, reason as u32).to_variant());
        changed(Change::Removed(id));
    }
}

fn emit(signal: &str, args: glib::Variant) {
    CONNECTION.with(|c| {
        if let Some(connection) = &*c.borrow() {
            let _ = connection.emit_signal(None, PATH, NAME, signal, Some(&args));
        }
    });
}

/// Claims the notification service. If another daemon holds it, this one
/// shows nothing.
pub fn start() {
    let info = Rc::new(gio::DBusNodeInfo::for_xml(XML).expect("notifications introspection"));
    gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::NONE,
        move |connection, _| {
            CONNECTION.with(|c| *c.borrow_mut() = Some(connection.clone()));
            let iface = info.lookup_interface(NAME).expect("notifications interface");
            let registered = connection
                .register_object(PATH, &iface)
                .method_call(|_, _, _, _, method, params, invocation| match method {
                    "GetCapabilities" => invocation.return_value(Some(&(vec!["actions", "body", "icon-static", "persistence"],).to_variant())),
                    "GetServerInformation" => invocation.return_value(Some(&("eclipse-shell", "eclipse-shell", env!("CARGO_PKG_VERSION"), "1.2").to_variant())),
                    "Notify" => {
                        let id = notify(&params);
                        invocation.return_value(Some(&(id,).to_variant()));
                    }
                    "CloseNotification" => {
                        if let Some((id,)) = params.get::<(u32,)>() {
                            close(id, Reason::Closed);
                        }
                        invocation.return_value(None);
                    }
                    _ => invocation.return_value(None),
                })
                .build();
            if let Err(e) = registered {
                eprintln!("eclipse-shell: notifications: {e}");
            }
        },
        |_, _| {},
        |_, _| eprintln!("eclipse-shell: another notification daemon owns {NAME}"),
    );
}

fn notify(params: &glib::Variant) -> u32 {
    let text = |i: usize| params.child_value(i).str().unwrap_or_default().to_owned();
    let (app, replaces, app_icon, summary, body) = (text(0), params.child_value(1).get::<u32>().unwrap_or(0), text(2), text(3), text(4));
    let flat = params.child_value(5).get::<Vec<String>>().unwrap_or_default();
    let actions = flat.chunks(2).filter_map(|pair| Some((pair.first()?.clone(), pair.get(1).cloned().unwrap_or_default()))).collect();
    let hints = glib::VariantDict::new(Some(&params.child_value(6)));
    let timeout = params.child_value(7).get::<i32>().unwrap_or(-1);

    let urgency = match hints.lookup_value("urgency", None).and_then(|v| v.get::<u8>().or_else(|| v.get::<u32>().map(|u| u as u8))) {
        Some(0) => Urgency::Low,
        Some(2) => Urgency::Critical,
        _ => Urgency::Normal,
    };
    let flag = |key: &str| hints.lookup_value(key, None).and_then(|v| v.get::<bool>()).unwrap_or(false);
    let hint_text = |key: &str| hints.lookup_value(key, None).and_then(|v| v.str().map(String::from)).filter(|s| !s.is_empty());
    let desktop_entry = hint_text("desktop-entry").unwrap_or_default();
    let picture = ["image-data", "image_data", "icon_data"]
        .iter()
        .find_map(|key| hints.lookup_value(key, None).and_then(|v| image_data(&v)))
        .map(Picture::Texture)
        .or_else(|| hint_text("image-path").or_else(|| hint_text("image_path")).and_then(|p| named_picture(&p)))
        .or_else(|| named_picture(&app_icon))
        .or_else(|| (!desktop_entry.is_empty()).then(|| Picture::Icon(crate::icons::for_class(&desktop_entry))));

    let replacing = replaces != 0 && get(replaces).is_some();
    let id = if replacing {
        replaces
    } else {
        NEXT_ID.with(|n| {
            let id = n.get();
            n.set(id.wrapping_add(1).max(1));
            id
        })
    };
    let notification = Notification {
        id,
        app: if app.is_empty() { "System".into() } else { app },
        summary: plain(&summary),
        body: plain(&body),
        actions,
        urgency,
        picture,
        desktop_entry,
        arrived: glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("a clock")),
        timeout: match timeout {
            t if t < 0 => None,
            t => Some(t as u32),
        },
        transient: flag("transient"),
        resident: flag("resident"),
        read: false,
    };
    LIST.with(|l| {
        let mut l = l.borrow_mut();
        match l.iter_mut().find(|n| n.id == id) {
            Some(slot) => *slot = notification,
            None => l.push(notification),
        }
    });
    changed(if replacing { Change::Replaced(id) } else { Change::Added(id) });
    id
}

/// An image-data hint, (iiibiiay): width, height, rowstride, has_alpha,
/// bits per sample, channels, pixels.
fn image_data(v: &glib::Variant) -> Option<gdk::Texture> {
    if v.n_children() < 7 {
        return None;
    }
    let (w, h, stride) = (v.child_value(0).get::<i32>()?, v.child_value(1).get::<i32>()?, v.child_value(2).get::<i32>()?);
    let alpha = v.child_value(3).get::<bool>()?;
    let pixels = v.child_value(6).fixed_array::<u8>().ok()?.to_vec();
    if w <= 0 || h <= 0 || stride <= 0 || pixels.len() < (stride * (h - 1)) as usize {
        return None;
    }
    let format = if alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
    Some(gdk::MemoryTexture::new(w, h, format, &glib::Bytes::from_owned(pixels), stride as usize).into())
}

/// An icon given by name, path or file:// URI.
fn named_picture(name: &str) -> Option<Picture> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let path = name.strip_prefix("file://").unwrap_or(name);
    if path.starts_with('/') {
        return gdk::Texture::from_filename(path).ok().map(Picture::Texture);
    }
    Some(Picture::Icon(gio::ThemedIcon::new(name).upcast()))
}

/// Text with markup tags dropped and entities decoded.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&amp;", "&").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn drops_markup() {
        assert_eq!(plain("<b>Build</b> finished &amp; passed"), "Build finished & passed");
        assert_eq!(plain("a &lt; b"), "a < b");
        assert_eq!(plain("  no markup  "), "no markup");
    }
}
