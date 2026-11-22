//! The lens row above the mode bar: a short row of round buttons, one per
//! lens and zoom stage (".5", "1", "2", "5"), and the macro lens's flower;
//! the button showing now carries the exact zoom ("1.4×"). A tap chooses a
//! lens or stage. Dragging sideways across the row turns it into the zoom
//! dial ([`crate::dial`]) for a stepless zoom; the dial stays a moment
//! after the finger lifts and can be dragged again.
//!
//! At its end the row holds the adjust button (the manual controls).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use omacam_core::controller::ZoomButton;

use crate::app::{format_zoom, stage_label};
use crate::dial::{self, ZoomDial};

/// How long the dial stays after the finger lifts.
const DIAL_STAYS: Duration = Duration::from_millis(1500);
/// How far a finger moves sideways before a drag becomes a zoom.
const DRAG_START: f64 = 10.0;

/// What the user did in the row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LensEvent {
    /// A lens or stage button: its zoom.
    Button(f64),
    /// The macro lens's button.
    Macro,
    /// The dial moved to this zoom.
    Drag(f64),
    /// The finger left the dial.
    Released,
}

pub struct LensRow {
    pub root: gtk::Stack,
    pill: gtk::Box,
    buttons: RefCell<Vec<(ZoomButton, gtk::Button)>>,
    macro_button: gtk::Button,
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

fn side_button(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::new();
    b.set_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("side-button");
    b.add_css_class("turnable");
    b.set_valign(gtk::Align::Center);
    b.set_size_request(44, 44);
    b
}

impl LensRow {
    pub fn new(on_event: impl Fn(LensEvent) + 'static) -> Rc<LensRow> {
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        pill.add_css_class("lens-pill");
        pill.set_halign(gtk::Align::Center);
        pill.set_valign(gtk::Align::Center);
        let macro_button = gtk::Button::from_icon_name("omacam-macro-symbolic");
        macro_button.add_css_class("lens-button");
        macro_button.add_css_class("turnable");
        macro_button.set_tooltip_text(Some("Macro lens: sharp a few centimeters away"));
        macro_button.set_visible(false);
        let adjust = side_button("omacam-adjust-symbolic", "Manual controls");
        let row = gtk::CenterBox::new();
        row.set_center_widget(Some(&pill));
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
        let lr = Rc::new(LensRow {
            root,
            pill,
            buttons: RefCell::new(Vec::new()),
            macro_button,
            adjust,
            dial,
            on_event: Rc::new(on_event),
            range: Cell::new((1.0, 1.0, 1.0)),
            stops: RefCell::new(Vec::new()),
            drag_base: Cell::new(1.0),
            dragging: Cell::new(false),
            hide: RefCell::new(None),
        });
        let e = lr.on_event.clone();
        lr.macro_button
            .connect_clicked(move |_| e(LensEvent::Macro));
        lr.connect_drag();
        lr
    }

    /// A sideways drag that starts on the buttons or the dial zooms.
    fn connect_drag(self: &Rc<Self>) {
        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let lr = Rc::downgrade(self);
        drag.connect_drag_begin(move |g, x, y| {
            let Some(lr) = lr.upgrade() else { return };
            let on_dial = lr.root.visible_child_name().as_deref() == Some("dial");
            let on_pill = lr
                .pill
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

    /// Bring the row in line with the controller's view: the buttons, the
    /// one showing now with the zoom, the macro button, the dial's range.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        buttons: &[ZoomButton],
        zoom: f64,
        floor: f64,
        max: f64,
        can_macro: bool,
        macro_on: bool,
        sensitive: bool,
    ) {
        let same = {
            let built = self.buttons.borrow();
            built.len() == buttons.len()
                && built
                    .iter()
                    .zip(buttons)
                    .all(|((a, _), b)| a.zoom == b.zoom && a.lens == b.lens)
        };
        if !same {
            self.build(buttons);
        }
        for ((_, b), now) in self.buttons.borrow_mut().iter_mut().zip(buttons) {
            if now.active {
                b.add_css_class("active");
                b.set_label(&format_zoom(zoom));
            } else {
                b.remove_css_class("active");
                b.set_label(&stage_label(now.zoom));
            }
            b.set_sensitive(sensitive);
        }
        for (built, now) in self.buttons.borrow_mut().iter_mut().zip(buttons) {
            built.0 = *now;
        }
        self.macro_button.set_visible(can_macro);
        self.macro_button.set_sensitive(sensitive);
        if macro_on {
            self.macro_button.add_css_class("active");
            self.macro_button
                .set_tooltip_text(Some(&format!("Macro lens, {}", format_zoom(zoom))));
        } else {
            self.macro_button.remove_css_class("active");
            self.macro_button
                .set_tooltip_text(Some("Macro lens: sharp a few centimeters away"));
        }
        self.range.set((zoom, floor, max));
        // The dial marks the stages within its range; with the macro lens
        // on, its own whole stops.
        let mut stops: Vec<f64> = if macro_on {
            vec![1.0, 2.0, 3.0, 4.0]
        } else {
            buttons.iter().map(|b| b.zoom).collect()
        };
        stops.retain(|s| *s >= floor - 1e-6 && *s <= max + 1e-6);
        if !stops.iter().any(|s| (s - floor).abs() < 1e-6) {
            stops.insert(0, floor);
        }
        self.dial.set(zoom, (floor, max), &stops);
        *self.stops.borrow_mut() = stops;
    }

    fn build(&self, buttons: &[ZoomButton]) {
        while let Some(c) = self.pill.first_child() {
            self.pill.remove(&c);
        }
        let mut built = Vec::new();
        for zb in buttons {
            let b = gtk::Button::with_label(&stage_label(zb.zoom));
            b.add_css_class("lens-button");
            b.add_css_class("turnable");
            if !zb.native {
                b.add_css_class("stage");
            }
            b.set_tooltip_text(Some(&format_zoom(zb.zoom)));
            let (e, z) = (self.on_event.clone(), zb.zoom);
            b.connect_clicked(move |_| e(LensEvent::Button(z)));
            self.pill.append(&b);
            built.push((*zb, b));
        }
        self.pill.append(&self.macro_button);
        *self.buttons.borrow_mut() = built;
    }
}
