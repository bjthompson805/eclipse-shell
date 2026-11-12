//! System tray over D-Bus (StatusNotifierItem): owns the watcher that tray
//! apps register with, keeps each item's icon, title and menu path current,
//! and talks to items for clicks and menus (com.canonical.dbusmenu).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";
const WATCHER_XML: &str = r#"
<node>
  <interface name="org.kde.StatusNotifierWatcher">
    <method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
    <method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
    <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
    <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
    <property name="ProtocolVersion" type="i" access="read"/>
    <signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
    <signal name="StatusNotifierItemUnregistered"><arg type="s"/></signal>
    <signal name="StatusNotifierHostRegistered"/>
  </interface>
</node>"#;

#[derive(Clone, Default)]
pub struct Item {
    pub service: String,
    pub path: String,
    pub icon_name: String,
    pub icon_theme_path: String,
    /// Largest-first (width, height, ARGB32 big-endian pixels).
    pub pixmaps: Vec<(i32, i32, Vec<u8>)>,
    pub title: String,
    pub menu: Option<String>,
    pub item_is_menu: bool,
}

impl Item {
    fn key(&self) -> String {
        format!("{}{}", self.service, self.path)
    }
}

thread_local! {
    static CONNECTION: RefCell<Option<gio::DBusConnection>> = RefCell::default();
    static ITEMS: RefCell<Vec<Item>> = RefCell::default();
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = RefCell::default();
    // Keeps name watches and signal subscriptions alive per item key.
    // (gio's bus-name watcher id can't be named, so a closure ends the watch.)
    static WATCHES: RefCell<Vec<(String, Box<dyn FnOnce()>, gio::SignalSubscription)>> = RefCell::default();
}

pub fn items() -> Vec<Item> {
    ITEMS.with(|i| i.borrow().clone())
}

pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

fn changed() {
    LISTENERS.with(|l| l.borrow().iter().for_each(|f| f()));
}

/// Claims the watcher name on the session bus. Another tray already holding
/// it keeps it; this one then shows nothing.
pub fn start() {
    let info = gio::DBusNodeInfo::for_xml(WATCHER_XML).expect("watcher introspection");
    let info = Rc::new(info);
    gio::bus_own_name(
        gio::BusType::Session,
        WATCHER_NAME,
        gio::BusNameOwnerFlags::NONE,
        move |connection, _| {
            CONNECTION.with(|c| *c.borrow_mut() = Some(connection.clone()));
            let iface = info.lookup_interface("org.kde.StatusNotifierWatcher").expect("watcher interface");
            let registered = connection
                .register_object(WATCHER_PATH, &iface)
                .method_call(|connection, sender, _, _, method, params, invocation| {
                    if method == "RegisterStatusNotifierItem" {
                        let arg = params.child_value(0).str().unwrap_or_default().to_owned();
                        register(&connection, sender.unwrap_or_default(), &arg);
                    }
                    invocation.return_value(None);
                })
                .property(|_, _, _, _, property| match property {
                    "RegisteredStatusNotifierItems" => {
                        ITEMS.with(|i| i.borrow().iter().map(Item::key).collect::<Vec<_>>()).to_variant()
                    }
                    "IsStatusNotifierHostRegistered" => true.to_variant(),
                    _ => 0i32.to_variant(),
                })
                .build();
            if let Err(e) = registered {
                eprintln!("eclipse-shell: tray watcher: {e}");
            }
        },
        |connection, _| {
            let _ = connection.emit_signal(None, WATCHER_PATH, WATCHER_NAME, "StatusNotifierHostRegistered", None);
        },
        |_, _| eprintln!("eclipse-shell: another tray owns {WATCHER_NAME}; the tray stays empty"),
    );
}

/// An item registers with either its bus name or its object path (then the
/// caller's unique name is the service).
fn register(connection: &gio::DBusConnection, sender: &str, arg: &str) {
    let (service, path) = if arg.starts_with('/') {
        (sender.to_owned(), arg.to_owned())
    } else {
        (arg.to_owned(), "/StatusNotifierItem".to_owned())
    };
    let item = Item { service: service.clone(), path: path.clone(), ..Item::default() };
    let key = item.key();
    if ITEMS.with(|i| i.borrow().iter().any(|it| it.key() == key)) {
        return;
    }
    ITEMS.with(|i| i.borrow_mut().push(item));

    let watch = gio::bus_watch_name_on_connection(connection, &service, gio::BusNameWatcherFlags::NONE, |_, _, _| {}, {
        let key = key.clone();
        move |connection, _| unregister(&connection, &key)
    });
    let subscription = connection.subscribe_to_signal(
        Some(&service),
        Some(ITEM_IFACE),
        None,
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        {
            let key = key.clone();
            move |_| refresh(&key)
        },
    );
    let unwatch: Box<dyn FnOnce()> = Box::new(move || gio::bus_unwatch_name(watch));
    WATCHES.with(|w| w.borrow_mut().push((key.clone(), unwatch, subscription)));
    let _ = connection.emit_signal(None, WATCHER_PATH, WATCHER_NAME, "StatusNotifierItemRegistered", Some(&(key.as_str(),).to_variant()));
    refresh(&key);
}

fn unregister(connection: &gio::DBusConnection, key: &str) {
    ITEMS.with(|i| i.borrow_mut().retain(|it| it.key() != key));
    let gone: Vec<_> = WATCHES.with(|w| {
        let mut w = w.borrow_mut();
        let (gone, kept) = w.drain(..).partition(|(k, _, _)| k == key);
        *w = kept;
        gone
    });
    for (_, unwatch, _) in gone {
        unwatch();
    }
    let _ = connection.emit_signal(None, WATCHER_PATH, WATCHER_NAME, "StatusNotifierItemUnregistered", Some(&(key,).to_variant()));
    changed();
}

/// Re-reads every property of one item.
fn refresh(key: &str) {
    let Some(connection) = CONNECTION.with(|c| c.borrow().clone()) else { return };
    let Some(item) = ITEMS.with(|i| i.borrow().iter().find(|it| it.key() == key).cloned()) else { return };
    let key = key.to_owned();
    glib::spawn_future_local(async move {
        let reply = connection
            .call_future(
                Some(&item.service),
                &item.path,
                "org.freedesktop.DBus.Properties",
                "GetAll",
                Some(&(ITEM_IFACE,).to_variant()),
                Some(glib::VariantTy::new("(a{sv})").unwrap()),
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await;
        let Ok(reply) = reply else { return };
        let props = glib::VariantDict::new(Some(&reply.child_value(0)));
        let string = |k: &str| props.lookup::<String>(k).ok().flatten().unwrap_or_default();
        let status = string("Status");
        let attention = status == "NeedsAttention" && !string("AttentionIconName").is_empty();
        let mut pixmaps = Vec::new();
        if let Some(raw) = props.lookup_value(if attention { "AttentionIconPixmap" } else { "IconPixmap" }, None) {
            for i in 0..raw.n_children() {
                let p = raw.child_value(i);
                let (w, h) = (p.child_value(0).get::<i32>().unwrap_or(0), p.child_value(1).get::<i32>().unwrap_or(0));
                let bytes = p.child_value(2).fixed_array::<u8>().map(<[u8]>::to_vec).unwrap_or_default();
                if w > 0 && h > 0 && bytes.len() >= (w * h * 4) as usize {
                    pixmaps.push((w, h, bytes));
                }
            }
        }
        pixmaps.sort_by_key(|(w, _, _)| -w);
        let updated = Item {
            icon_name: string(if attention { "AttentionIconName" } else { "IconName" }),
            icon_theme_path: string("IconThemePath"),
            pixmaps,
            title: string("Title"),
            menu: props.lookup_value("Menu", None).and_then(|m| m.str().map(String::from)).filter(|m| m != "/"),
            item_is_menu: props.lookup::<bool>("ItemIsMenu").ok().flatten().unwrap_or(false),
            ..item
        };
        let found = ITEMS.with(|i| {
            let mut i = i.borrow_mut();
            i.iter_mut().find(|it| it.key() == key).map(|it| *it = updated).is_some()
        });
        if found {
            changed();
        }
    });
}

fn call(service: &str, path: &str, iface: &str, method: &str, args: glib::Variant) {
    let Some(connection) = CONNECTION.with(|c| c.borrow().clone()) else { return };
    connection.call(Some(service), path, iface, method, Some(&args), None, gio::DBusCallFlags::NONE, 5000, gio::Cancellable::NONE, |_| {});
}

pub fn activate(item: &Item, x: i32, y: i32) {
    call(&item.service, &item.path, ITEM_IFACE, "Activate", (x, y).to_variant());
}

pub fn secondary_activate(item: &Item, x: i32, y: i32) {
    call(&item.service, &item.path, ITEM_IFACE, "SecondaryActivate", (x, y).to_variant());
}

pub fn scroll(item: &Item, delta: i32) {
    call(&item.service, &item.path, ITEM_IFACE, "Scroll", (delta, "vertical").to_variant());
}

/// One entry of an item's menu.
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub separator: bool,
    pub enabled: bool,
    /// Some(checked) for check and radio entries.
    pub toggle: Option<bool>,
    pub children: Vec<MenuEntry>,
}

/// Fetches an item's menu tree, after telling it the menu is about to show.
pub async fn menu(item: &Item) -> Option<Vec<MenuEntry>> {
    let connection = CONNECTION.with(|c| c.borrow().clone())?;
    let path = item.menu.as_deref()?;
    let _ = connection
        .call_future(Some(&item.service), path, MENU_IFACE, "AboutToShow", Some(&(0i32,).to_variant()), None, gio::DBusCallFlags::NONE, 2000)
        .await;
    let reply = connection
        .call_future(
            Some(&item.service),
            path,
            MENU_IFACE,
            "GetLayout",
            Some(&(0i32, -1i32, Vec::<String>::new()).to_variant()),
            Some(glib::VariantTy::new("(u(ia{sv}av))").unwrap()),
            gio::DBusCallFlags::NONE,
            5000,
        )
        .await
        .ok()?;
    Some(parse_node(&reply.child_value(1)).children)
}

fn parse_node(node: &glib::Variant) -> MenuEntry {
    let props = glib::VariantDict::new(Some(&node.child_value(1)));
    let children = node.child_value(2);
    let toggle_type = props.lookup::<String>("toggle-type").ok().flatten().unwrap_or_default();
    MenuEntry {
        id: node.child_value(0).get().unwrap_or(0),
        label: props.lookup::<String>("label").ok().flatten().unwrap_or_default(),
        separator: props.lookup::<String>("type").ok().flatten().as_deref() == Some("separator"),
        enabled: props.lookup::<bool>("enabled").ok().flatten().unwrap_or(true),
        toggle: (!toggle_type.is_empty()).then(|| props.lookup::<i32>("toggle-state").ok().flatten() == Some(1)),
        children: (0..children.n_children())
            .map(|i| children.child_value(i).as_variant().expect("av child"))
            .filter(|child| {
                let p = glib::VariantDict::new(Some(&child.child_value(1)));
                p.lookup::<bool>("visible").ok().flatten().unwrap_or(true)
            })
            .map(|child| parse_node(&child))
            .collect(),
    }
}

pub fn menu_clicked(item: &Item, id: i32) {
    let Some(path) = item.menu.as_deref() else { return };
    let now = (glib::real_time() / 1_000_000) as u32;
    call(&item.service, path, MENU_IFACE, "Event", (id, "clicked", 0i32.to_variant(), now).to_variant());
}
