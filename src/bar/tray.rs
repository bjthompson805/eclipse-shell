//! Tray icons, one per registered item, brightening on hover. Left click
//! activates the item (or opens its menu, for menu-only items), middle click
//! is the secondary action, right click opens the menu, and scrolling goes
//! to the item.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;

use crate::hint;
use crate::tray::{self, Item, MenuEntry};

const REST_OPACITY: f64 = 0.78;

/// Each icon's title shows on `hints` at `hint_y` while it's hovered.
pub fn tray(s: f64, hints: &gtk::Fixed, hint_y: f64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, (9.0 * s).round() as i32);
    let rebuild = {
        let (row, hints) = (row.downgrade(), hints.downgrade());
        move || {
            let Some(hints) = hints.upgrade() else { return };
            let Some(row) = row.upgrade() else { return };
            while let Some(child) = row.first_child() {
                row.remove(&child);
            }
            let items = tray::items();
            for item in &items {
                let icon = icon(s, item);
                if !item.title.is_empty() {
                    hint::float_under(&icon, &hints, s, hint_y, &item.title);
                }
                row.append(&icon);
            }
            row.set_visible(!items.is_empty());
        }
    };
    rebuild();
    tray::on_change(rebuild);
    row
}

fn icon(s: f64, item: &Item) -> gtk::Image {
    let px = (16.0 * s).round() as i32;
    let image = gtk::Image::new();
    image.set_pixel_size(px);
    image.set_size_request(px, px);
    image.set_opacity(REST_OPACITY);
    image.set_cursor_from_name(Some("pointer"));
    match texture(item, px * 2) {
        Some(paintable) => image.set_paintable(Some(&paintable)),
        None => image.set_icon_name(Some("application-x-executable")),
    }

    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter({
        let image = image.downgrade();
        move |_, _, _| image.upgrade().into_iter().for_each(|i| i.set_opacity(1.0))
    });
    motion.connect_leave({
        let image = image.downgrade();
        move |_| image.upgrade().into_iter().for_each(|i| i.set_opacity(REST_OPACITY))
    });
    image.add_controller(motion);

    let click = gtk::GestureClick::builder().button(0).build();
    click.connect_released({
        let item = item.clone();
        move |gesture, _, _, _| {
            let Some(widget) = gesture.widget() else { return };
            let (x, y) = screen_point(&widget);
            match gesture.current_button() {
                gdk::BUTTON_PRIMARY if item.item_is_menu && item.menu.is_some() => open_menu(&widget, &item),
                gdk::BUTTON_PRIMARY => tray::activate(&item, x, y),
                gdk::BUTTON_MIDDLE => tray::secondary_activate(&item, x, y),
                gdk::BUTTON_SECONDARY if item.menu.is_some() => open_menu(&widget, &item),
                gdk::BUTTON_SECONDARY => tray::activate(&item, x, y),
                _ => {}
            }
        }
    });
    image.add_controller(click);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.connect_scroll({
        let item = item.clone();
        move |_, _, dy| {
            tray::scroll(&item, (-dy * 120.0) as i32);
            glib::Propagation::Stop
        }
    });
    image.add_controller(scroll);
    image
}

/// The item's icon: a themed name (looking in the item's own theme
/// directory too), a file path, or the pixel data it sent.
fn texture(item: &Item, px: i32) -> Option<gdk::Paintable> {
    if item.icon_name.starts_with('/') {
        return gdk::Texture::from_filename(&item.icon_name).ok().map(|t| t.upcast());
    }
    if !item.icon_name.is_empty() {
        let display = gdk::Display::default()?;
        let theme = gtk::IconTheme::for_display(&display);
        if !item.icon_theme_path.is_empty() && !theme.search_path().iter().any(|p| p.to_str() == Some(&item.icon_theme_path)) {
            theme.add_search_path(&item.icon_theme_path);
        }
        if theme.has_icon(&item.icon_name) {
            let icon = theme.lookup_icon(&item.icon_name, &[], px, 1, gtk::TextDirection::None, gtk::IconLookupFlags::empty());
            return Some(icon.upcast());
        }
    }
    // Pixmaps are ARGB in network byte order; GDK wants RGBA.
    let (w, h, argb) = item.pixmaps.iter().find(|(w, _, _)| *w <= px).or(item.pixmaps.last())?;
    let rgba: Vec<u8> = argb.chunks_exact(4).flat_map(|p| [p[1], p[2], p[3], p[0]]).collect();
    let texture = gdk::MemoryTexture::new(*w, *h, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(rgba), (*w * 4) as usize);
    Some(texture.upcast())
}

fn screen_point(widget: &gtk::Widget) -> (i32, i32) {
    let bounds = widget.compute_bounds(&widget.root().map(|r| r.upcast::<gtk::Widget>()).unwrap_or(widget.clone()));
    bounds.map_or((0, 0), |b| ((b.x() + b.width() / 2.0) as i32, (b.y() + b.height()) as i32))
}

/// Builds the item's menu as a popover under its icon.
fn open_menu(widget: &gtk::Widget, item: &Item) {
    let (widget, item) = (widget.clone(), item.clone());
    glib::spawn_future_local(async move {
        let Some(entries) = tray::menu(&item).await else { return };
        let actions = gio::SimpleActionGroup::new();
        let model = gio::Menu::new();
        fill(&model, &entries, &actions, &item);
        widget.insert_action_group("tray", Some(&actions));

        let popover = gtk::PopoverMenu::from_model(Some(&model));
        popover.set_has_arrow(false);
        popover.set_parent(&widget);
        popover.connect_closed(|p| {
            let p = p.clone();
            // Unparent after the click that closed it has been handled.
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    });
}

/// Separators split the menu into sections, which GTK draws with a rule.
fn fill(menu: &gio::Menu, entries: &[MenuEntry], actions: &gio::SimpleActionGroup, item: &Item) {
    let mut section = gio::Menu::new();
    for entry in entries {
        if entry.separator {
            if section.n_items() > 0 {
                menu.append_section(None, &section);
            }
            section = gio::Menu::new();
            continue;
        }
        let name = format!("item-{}", entry.id);
        let detailed = format!("tray.{name}");
        if entry.children.is_empty() {
            let action = match entry.toggle {
                Some(on) => gio::SimpleAction::new_stateful(&name, None, &on.to_variant()),
                None => gio::SimpleAction::new(&name, None),
            };
            action.set_enabled(entry.enabled);
            action.connect_activate({
                let (item, id) = (item.clone(), entry.id);
                move |_, _| tray::menu_clicked(&item, id)
            });
            actions.add_action(&action);
            section.append(Some(&entry.label), Some(&detailed));
        } else {
            let submenu = gio::Menu::new();
            fill(&submenu, &entry.children, actions, item);
            section.append_submenu(Some(&entry.label), &submenu);
        }
    }
    if section.n_items() > 0 {
        menu.append_section(None, &section);
    }
}
