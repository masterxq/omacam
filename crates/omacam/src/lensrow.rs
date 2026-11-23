//! The lens row above the mode bar: the zoom field, the lens button
//! beside it, and the adjust button at the end.
//!
//! The zoom field shows the zoom against the main camera's view ("1.0×",
//! the ultrawide's own view "0.5×"; the macro lens's own view is its
//! 1×). A tap on it,
//! or a sideways drag across it, opens the zoom dial ([`crate::dial`]) in
//! its place: drag it for a stepless zoom, or tap one of its marks (the
//! lens's least and most zoom, and 1×) to go there. The dial closes a
//! moment after the last touch, or at a tap elsewhere. Zooming never
//! changes the lens: the lens button does. It shows the lens in use, and a
//! tap drops the lens menu down (up, here) over the row, each lens with
//! its symbol and its name.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use omacam_core::controller::LensChoice;
use omacam_core::lens::Role;

use crate::dial::{self, ZoomDial};

/// How long the dial stays after the last touch.
const DIAL_STAYS: Duration = Duration::from_millis(2500);
/// How far a finger moves sideways before a drag becomes a zoom.
const DRAG_START: f64 = 10.0;

/// What the user did in the row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LensEvent {
    /// A lens from the lens menu (its index in the lens table).
    Lens(usize),
    /// The dial moved to this zoom.
    Drag(f64),
    /// The finger left the dial.
    Released,
}

/// A lens's symbol.
pub fn lens_icon(role: Role) -> &'static str {
    match role {
        Role::Main => "omacam-lens-main-symbolic",
        Role::Ultrawide => "omacam-lens-wide-symbolic",
        Role::Tele => "omacam-lens-tele-symbolic",
        Role::Macro => "omacam-macro-symbolic",
        Role::Front => "omacam-flip-symbolic",
    }
}

/// A lens's name, as the menu and the note over the picture say it.
pub fn lens_name(role: Role) -> &'static str {
    match role {
        Role::Main => "Main",
        Role::Ultrawide => "Wide",
        Role::Tele => "Tele",
        Role::Macro => "Macro",
        Role::Front => "Front",
    }
}

pub struct LensRow {
    pub root: gtk::Stack,
    /// Shows the zoom; a tap opens the dial.
    pub zoom_field: gtk::Button,
    /// Shows the lens in use; a tap opens [`LensRow::menu`].
    pub lens_button: gtk::Button,
    lens_icon: gtk::Image,
    /// The lens menu, over the row when the lens button is tapped.
    pub menu: gtk::Revealer,
    menu_box: gtk::Box,
    choices: RefCell<Vec<(LensChoice, gtk::Button)>>,
    /// Opens the manual controls.
    pub adjust: gtk::ToggleButton,
    dial: ZoomDial,
    on_event: Rc<dyn Fn(LensEvent)>,
    /// The zoom now, the lowest a drag reaches, the highest.
    range: Cell<(f64, f64, f64)>,
    stops: RefCell<Vec<f64>>,
    drag_base: Cell<f64>,
    dragging: Cell<bool>,
    hide: RefCell<Option<glib::SourceId>>,
}

impl LensRow {
    pub fn new(on_event: impl Fn(LensEvent) + 'static) -> Rc<LensRow> {
        let zoom_field = gtk::Button::with_label("1.0×");
        zoom_field.add_css_class("zoom-field");
        zoom_field.add_css_class("turnable");
        zoom_field.set_tooltip_text(Some("Zoom"));
        zoom_field.set_valign(gtk::Align::Center);
        let lens_icon = gtk::Image::from_icon_name(lens_icon(Role::Main));
        let lens_button = gtk::Button::new();
        lens_button.set_child(Some(&lens_icon));
        lens_button.add_css_class("lens-choice");
        lens_button.add_css_class("turnable");
        lens_button.set_tooltip_text(Some("Lens"));
        lens_button.set_valign(gtk::Align::Center);
        lens_button.set_size_request(44, 44);
        lens_button.set_visible(false);
        let group = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        group.append(&zoom_field);
        group.append(&lens_button);
        let adjust = gtk::ToggleButton::new();
        adjust.set_icon_name("omacam-adjust-symbolic");
        adjust.set_tooltip_text(Some("Manual controls"));
        adjust.add_css_class("side-button");
        adjust.add_css_class("turnable");
        adjust.set_valign(gtk::Align::Center);
        adjust.set_size_request(44, 44);
        // As wide as the adjust button at the start, so the zoom field and
        // the lens button stay in the middle.
        let balance = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        balance.set_size_request(44, -1);
        let row = gtk::CenterBox::new();
        row.set_start_widget(Some(&balance));
        row.set_center_widget(Some(&group));
        row.set_end_widget(Some(&adjust));
        row.add_css_class("lens-row");
        let dial = ZoomDial::new();
        dial.add_css_class("zoom-dial");
        let root = gtk::Stack::new();
        root.set_transition_type(gtk::StackTransitionType::Crossfade);
        root.set_transition_duration(120);
        root.add_named(&row, Some("row"));
        root.add_named(&dial, Some("dial"));
        root.set_visible_child_name("row");
        let menu_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        menu_box.add_css_class("flash-menu");
        let menu = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .child(&menu_box)
            .halign(gtk::Align::Center)
            .build();
        let lr = Rc::new(LensRow {
            root,
            zoom_field,
            lens_button,
            lens_icon,
            menu,
            menu_box,
            choices: RefCell::new(Vec::new()),
            adjust,
            dial,
            on_event: Rc::new(on_event),
            range: Cell::new((1.0, 1.0, 1.0)),
            stops: RefCell::new(Vec::new()),
            drag_base: Cell::new(1.0),
            dragging: Cell::new(false),
            hide: RefCell::new(None),
        });
        let weak = Rc::downgrade(&lr);
        lr.lens_button.connect_clicked(move |_| {
            if let Some(lr) = weak.upgrade() {
                lr.menu.set_reveal_child(!lr.menu.reveals_child());
            }
        });
        let weak = Rc::downgrade(&lr);
        lr.zoom_field.connect_clicked(move |_| {
            if let Some(lr) = weak.upgrade() {
                lr.show_dial();
                lr.hide_dial_later();
            }
        });
        // A tap on one of the dial's marks goes there.
        let tap = gtk::GestureClick::new();
        let weak = Rc::downgrade(&lr);
        tap.connect_released(move |_, _, x, _| {
            let Some(lr) = weak.upgrade() else { return };
            if let Some(z) = lr.dial.mark_at(x) {
                (lr.on_event)(LensEvent::Drag(z));
                (lr.on_event)(LensEvent::Released);
            }
            lr.hide_dial_later();
        });
        lr.dial.add_controller(tap);
        lr.connect_drag();
        lr
    }

    /// The dial is open.
    pub fn dial_open(&self) -> bool {
        self.root.visible_child_name().as_deref() == Some("dial")
    }

    /// Close the dial at once (a tap elsewhere); whether it was open.
    pub fn hide_dial(&self) -> bool {
        if let Some(id) = self.hide.borrow_mut().take() {
            id.remove();
        }
        let open = self.dial_open();
        self.root.set_visible_child_name("row");
        open
    }

    /// Hold the dial's writing upright for a phone turned so.
    pub fn set_turn(&self, degrees: u32) {
        self.dial.set_turn(degrees);
    }

    /// A sideways drag that starts on the zoom field or the dial zooms.
    fn connect_drag(self: &Rc<Self>) {
        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let lr = Rc::downgrade(self);
        drag.connect_drag_begin(move |g, x, y| {
            let Some(lr) = lr.upgrade() else { return };
            let on_dial = lr.root.visible_child_name().as_deref() == Some("dial");
            let on_pill = lr
                .zoom_field
                .compute_bounds(&lr.root)
                .is_some_and(|b| b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32)));
            if !on_dial && !on_pill {
                g.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            lr.drag_base.set(lr.range.get().0);
            lr.dragging.set(false);
        });
        let lr = Rc::downgrade(self);
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(lr) = lr.upgrade() else { return };
            if !lr.dragging.get() {
                if dx.abs() < DRAG_START || dy.abs() > dx.abs() {
                    return;
                }
                g.set_state(gtk::EventSequenceState::Claimed);
                lr.dragging.set(true);
                lr.show_dial();
            }
            let (_, lo, hi) = lr.range.get();
            let z = dial::zoom_after_drag(lr.drag_base.get(), dx).clamp(lo, hi);
            let z = dial::snap(z, &lr.stops.borrow());
            (lr.on_event)(LensEvent::Drag(z));
        });
        let lr = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            let Some(lr) = lr.upgrade() else { return };
            if lr.dragging.replace(false) {
                (lr.on_event)(LensEvent::Released);
                lr.hide_dial_later();
            }
        });
        self.root.add_controller(drag);
    }

    /// Show the dial in place of the buttons (until [`DIAL_STAYS`] after
    /// the last drag).
    pub fn show_dial(&self) {
        if let Some(id) = self.hide.borrow_mut().take() {
            id.remove();
        }
        self.menu.set_reveal_child(false);
        self.root.set_visible_child_name("dial");
    }

    pub fn hide_dial_later(self: &Rc<Self>) {
        if let Some(id) = self.hide.borrow_mut().take() {
            id.remove();
        }
        let lr = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(DIAL_STAYS, move || {
            if let Some(lr) = lr.upgrade() {
                lr.hide.borrow_mut().take();
                if !lr.dragging.get() {
                    lr.root.set_visible_child_name("row");
                }
            }
        });
        *self.hide.borrow_mut() = Some(id);
    }

    /// Bring the row in line with the controller's view: the zoom, the
    /// one showing now with the zoom, the lens button and its menu, the
    /// dial's range.
    pub fn update(
        &self,
        view: &omacam_core::controller::ViewState,
        lens_sensitive: bool,
        sensitive: bool,
    ) {
        let (zoom, floor, max) = (view.zoom, view.zoom_floor, view.zoom_ceiling);
        let unit = if view.zoom_unit > 0.0 {
            view.zoom_unit
        } else {
            1.0
        };
        self.zoom_field
            .set_label(&format!("{:.1}×", (zoom / unit * 10.0).round() / 10.0));
        self.zoom_field.set_sensitive(sensitive);
        // The lens button and its menu.
        let same = {
            let built = self.choices.borrow();
            built.len() == view.lenses.len()
                && built
                    .iter()
                    .zip(&view.lenses)
                    .all(|((a, _), b)| a.lens == b.lens)
        };
        if !same {
            self.build_menu(&view.lenses);
        }
        let current = view.lenses.iter().find(|l| l.active);
        self.lens_button.set_visible(current.is_some());
        if current.is_none() {
            self.menu.set_reveal_child(false);
        }
        if let Some(c) = current {
            self.lens_icon.set_icon_name(Some(lens_icon(c.role)));
            self.lens_button
                .set_tooltip_text(Some(&format!("Lens: {}", lens_name(c.role))));
        }
        self.lens_button.set_sensitive(lens_sensitive);
        for ((_, b), now) in self.choices.borrow_mut().iter_mut().zip(&view.lenses) {
            if now.active {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
            // While recording the macro lens can't be chosen or left.
            let macro_part =
                now.role == Role::Macro || current.is_some_and(|c| c.role == Role::Macro);
            b.set_sensitive(lens_sensitive && !(view.recording && macro_part));
        }
        for (built, now) in self.choices.borrow_mut().iter_mut().zip(&view.lenses) {
            built.0 = *now;
        }
        self.range.set((zoom, floor, max));
        // The dial marks the lens's limits and 1× (the main camera's view,
        // where the lens reaches it).
        let mut stops = vec![floor, unit, max];
        stops.retain(|s| *s >= floor - 1e-6 && *s <= max + 1e-6);
        stops.sort_by(f64::total_cmp);
        stops.dedup_by(|a, b| (*a / *b - 1.0).abs() < 1e-6);
        self.dial.set(zoom, (floor, max), &stops, unit);
        *self.stops.borrow_mut() = stops;
    }

    fn build_menu(&self, lenses: &[LensChoice]) {
        while let Some(c) = self.menu_box.first_child() {
            self.menu_box.remove(&c);
        }
        let mut built = Vec::new();
        for l in lenses {
            let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
            col.append(&gtk::Image::from_icon_name(lens_icon(l.role)));
            col.append(&gtk::Label::new(Some(lens_name(l.role))));
            let b = gtk::Button::new();
            b.set_child(Some(&col));
            b.add_css_class("flash-choice");
            b.add_css_class("turnable");
            let (e, lens, menu) = (self.on_event.clone(), l.lens, self.menu.clone());
            b.connect_clicked(move |_| {
                menu.set_reveal_child(false);
                e(LensEvent::Lens(lens));
            });
            self.menu_box.append(&b);
            built.push((*l, b));
        }
        *self.choices.borrow_mut() = built;
    }

    /// The lens of this role from the menu, as a tap on its entry.
    pub fn choose(&self, role: Role) {
        let lens = self
            .choices
            .borrow()
            .iter()
            .find(|(c, _)| c.role == role)
            .map(|(c, _)| c.lens);
        if let Some(lens) = lens {
            self.menu.set_reveal_child(false);
            (self.on_event)(LensEvent::Lens(lens));
        }
    }
}
