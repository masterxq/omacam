//! Keeps the window's layout upright to the phone itself.
//!
//! On a phone the shell turns the whole screen when the phone is held
//! sideways (Phosh: phoc turns the output, and the app's window becomes
//! landscape). A camera keeps its layout where it is on the phone, with
//! the shutter at the same edge, and only turns its icons; the shell offers
//! no way to keep one window out of the turn. So [`Turned`] turns its
//! child back: in a window wider than tall it lays the child out in the
//! portrait shape (the window's height across, its width down) and turns
//! it a quarter so that the child's top lies at the phone's top. GTK draws
//! and picks input through the turn, so taps and drags land where they
//! show. Which way the quarter goes follows how the phone is held, the
//! shell's own cue: held with its left side up (turned 90° clockwise),
//! the phone's top is at the window's right.
//!
//! In a portrait window (the phone upright, or the shell's rotation
//! locked) the child is laid out as it is.

use std::cell::Cell;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};
use gtk4 as gtk;

type OnTurn = Box<dyn Fn(u32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Turned {
        pub child: std::cell::RefCell<Option<gtk::Widget>>,
        /// Clockwise degrees to turn the child in a landscape window: 90
        /// or 270.
        pub quarter: Cell<u32>,
        /// The turn of the last layout (0 in a portrait window).
        pub applied: Cell<u32>,
        /// Called (from the main loop) when the turn of the layout changes.
        pub on_turn: std::cell::RefCell<Option<OnTurn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Turned {
        const NAME: &'static str = "OmacamTurned";
        type Type = super::Turned;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Turned {
        fn dispose(&self) {
            if let Some(c) = self.child.borrow_mut().take() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for Turned {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            // Either way round, the child's minimum on its shorter side,
            // so the window can be turned.
            let Some(child) = self.child.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            let (wmin, _, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            let (hmin, _, _, _) = child.measure(gtk::Orientation::Vertical, -1);
            let min = wmin.min(hmin);
            (min, min, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            let turn = if width > height {
                self.quarter.get()
            } else {
                0
            };
            if self.applied.replace(turn) != turn {
                let obj = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(t) = obj.upgrade() {
                        if let Some(f) = t.imp().on_turn.borrow().as_ref() {
                            f(t.imp().applied.get());
                        }
                    }
                });
            }
            let (cw, ch) = if turn == 0 {
                (width, height)
            } else {
                (height, width)
            };
            let (dx, dy) = offset(turn, (width as f32, height as f32));
            let t = gsk::Transform::new()
                .translate(&graphene::Point::new(dx, dy))
                .rotate(turn as f32);
            child.allocate(cw, ch, -1, Some(t));
        }
    }
}

/// Where the child's origin goes in a `size` parent when it is turned
/// `turn` degrees clockwise (0, 90 or 270) about it.
fn offset(turn: u32, size: (f32, f32)) -> (f32, f32) {
    match turn {
        90 => (size.0, 0.0),
        270 => (0.0, size.1),
        _ => (0.0, 0.0),
    }
}

/// A child point to the parent's, turned `turn` degrees clockwise in a
/// `size` parent (what the allocation's transform does).
#[cfg(test)]
fn to_parent(turn: u32, size: (f32, f32), p: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = offset(turn, size);
    let (x, y) = match turn {
        90 => (-p.1, p.0),
        270 => (p.1, -p.0),
        _ => p,
    };
    (x + dx, y + dy)
}

glib::wrapper! {
    pub struct Turned(ObjectSubclass<imp::Turned>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Turned {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Turned {
        let t: Turned = glib::Object::builder()
            .property("hexpand", true)
            .property("vexpand", true)
            .build();
        t.imp().quarter.set(90);
        child.set_parent(&t);
        t.imp().child.replace(Some(child.clone().upcast()));
        t
    }

    /// How the phone is held, clockwise degrees from upright: a landscape
    /// window turns the child back by as much (90 or 270; the others leave
    /// the last sideways turn).
    pub fn set_held(&self, degrees: u32) {
        if (degrees == 90 || degrees == 270) && self.imp().quarter.replace(degrees) != degrees {
            self.queue_allocate();
        }
    }

    /// Call `f` with the layout's turn whenever it changes.
    pub fn connect_turn(&self, f: impl Fn(u32) + 'static) {
        self.imp().on_turn.replace(Some(Box::new(f)));
    }

    /// The turn of the last layout: 0, 90 or 270.
    pub fn turn(&self) -> u32 {
        self.imp().applied.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held with its left side up, the phone's top is at the right of the
    /// landscape window: the portrait child's top edge runs down the
    /// window's right edge, its bottom (the shutter) down the left.
    #[test]
    fn the_childs_top_lies_at_the_phones_top() {
        let size = (946.0, 415.0);
        // The child is 415 wide and 946 tall.
        assert_eq!(to_parent(90, size, (0.0, 0.0)), (946.0, 0.0));
        assert_eq!(to_parent(90, size, (415.0, 0.0)), (946.0, 415.0));
        assert_eq!(to_parent(90, size, (0.0, 946.0)), (0.0, 0.0));
        // Held the other way: the top at the left, from the bottom up.
        assert_eq!(to_parent(270, size, (0.0, 0.0)), (0.0, 415.0));
        assert_eq!(to_parent(270, size, (415.0, 946.0)), (946.0, 0.0));
        assert_eq!(to_parent(0, size, (3.0, 4.0)), (3.0, 4.0));
    }
}
