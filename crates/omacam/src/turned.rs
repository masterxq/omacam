//! Keeps the window's layout in its portrait shape when the shell turns
//! the screen.
//!
//! On a phone the shell turns the whole screen when the phone is held
//! sideways (Phosh: phoc turns the output, and the app's window becomes
//! landscape). A camera keeps its portrait layout, turned a quarter, with
//! the shutter at a long edge, and only turns its icons; the shell offers
//! no way to keep one window out of the turn. So [`Turned`] turns its
//! child: in a window wider than tall it lays the child out in the
//! portrait shape (the window's height across, its width down) and turns
//! it a quarter. GTK draws and picks input through the turn, so taps and
//! drags land where they show. Which way the quarter goes is a setting,
//! not how the phone is held: turned 270° clockwise (the default) the
//! child's bottom, the shutter, lies at the window's right, the same in
//! both landscape holds; turned 90° it lies at the left. In one of the
//! two holds the layout is thus upside down to the phone, and the picture
//! has to turn half round to stay upright ([`turns`]).
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
        /// Clockwise degrees to turn the child in a landscape window: 270
        /// (the shutter at the right) or 90 (at the left).
        pub quarter: Cell<u32>,
        /// The turn of the last layout (0 in a portrait window).
        pub applied: Cell<u32>,
        /// Called (from the main loop) when the turn of the layout changes.
        pub on_turn: std::cell::RefCell<Option<OnTurn>>,
        /// Called within the layout when its turn changes, before the
        /// turned child is drawn (for what may change while laid out).
        pub on_layout: std::cell::RefCell<Option<OnTurn>>,
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
                if let Some(f) = self.on_layout.borrow().as_ref() {
                    f(turn);
                }
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
        t.imp().quarter.set(270);
        child.set_parent(&t);
        t.imp().child.replace(Some(child.clone().upcast()));
        t
    }

    /// Which edge of a landscape window the child's bottom (the shutter)
    /// lies at: the left (turned 90° clockwise) or the right (270°).
    pub fn set_shutter_left(&self, left: bool) {
        let degrees = if left { 90 } else { 270 };
        if self.imp().quarter.replace(degrees) != degrees {
            self.queue_allocate();
        }
    }

    /// Call `f` with the layout's turn whenever it changes.
    pub fn connect_turn(&self, f: impl Fn(u32) + 'static) {
        self.imp().on_turn.replace(Some(Box::new(f)));
    }

    /// Call `f` with the layout's turn as it changes, within the layout:
    /// `f` may change how things are drawn, not their sizes or styles.
    pub fn connect_layout(&self, f: impl Fn(u32) + 'static) {
        self.imp().on_layout.replace(Some(Box::new(f)));
    }

    /// The turn of the last layout: 0, 90 or 270.
    pub fn turn(&self) -> u32 {
        self.imp().applied.get()
    }
}

/// How the picture and the controls turn inside a layout turned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turns {
    /// Clockwise degrees (0 or 180) the picture turns on top of turning
    /// the frame upright to the phone: half round when the layout lies
    /// upside down to the phone.
    pub picture: u32,
    /// Clockwise degrees the layout's icons turn back, so that they stand
    /// upright to how the phone is held.
    pub controls: u32,
}

/// The turns for a layout turned `layout` degrees clockwise in the window
/// (0 in a portrait window, else 90 or 270), on a screen the shell has
/// turned `screen` degrees (the output's transform, the same sense as
/// `held`; None where it can't be had), with the phone held `held`
/// degrees clockwise from upright (the accelerometer).
///
/// The camera sits fixed in the phone, so the frame is upright to the
/// layout as long as the layout is upright to the phone, as it is when
/// its turn in the window is the screen's. A screen turn that doesn't fit
/// the window's shape (a window landscape on an upright screen) is taken
/// as unknown; unknown, a landscape screen is the hold if that is
/// sideways, else the layout's own turn (the picture as before). Likewise
/// a landscape window with the phone held upright (no accelerometer)
/// counts as held as the screen is turned.
pub fn turns(layout: u32, screen: Option<u32>, held: u32) -> Turns {
    let landscape = layout % 180 == 90;
    let screen = match screen {
        Some(s) if (s % 180 == 90) == landscape => s % 360,
        _ if landscape && held % 180 == 90 => held,
        _ if landscape => layout,
        _ => 0,
    };
    let held = if landscape && held.is_multiple_of(180) {
        screen
    } else {
        held % 360
    };
    Turns {
        picture: (screen + 360 - layout) % 360,
        controls: (layout + held + 360 - screen) % 360,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upright, nothing turns; the screen locked upright with the phone
    /// on its side, only the icons turn.
    #[test]
    fn portrait_turns_only_the_icons_for_the_hold() {
        let t = turns(0, Some(0), 0);
        assert_eq!((t.picture, t.controls), (0, 0));
        let t = turns(0, Some(0), 90);
        assert_eq!((t.picture, t.controls), (0, 90));
        let t = turns(0, None, 270);
        assert_eq!((t.picture, t.controls), (0, 270));
    }

    /// The shutter at the right in both landscape holds: held with the
    /// left side up (the screen turned 90°) the layout lies upside down
    /// to the phone, and the picture turns half round; held the other
    /// way it is upright to the phone. Either way the icons turn back by
    /// the layout's turn, upright on the screen.
    #[test]
    fn the_shutter_stays_right_and_the_picture_upright() {
        let t = turns(270, Some(90), 90);
        assert_eq!((t.picture, t.controls), (180, 270));
        let t = turns(270, Some(270), 270);
        assert_eq!((t.picture, t.controls), (0, 270));
        // Left-handed: the mirror image.
        let t = turns(90, Some(90), 90);
        assert_eq!((t.picture, t.controls), (0, 90));
        let t = turns(90, Some(270), 270);
        assert_eq!((t.picture, t.controls), (180, 90));
    }

    /// The picture follows the screen, not the accelerometer: the screen
    /// locked in one landscape turn, the picture stays upright to the
    /// phone whichever way it is held, and the icons turn for the hold.
    #[test]
    fn the_picture_follows_the_screen() {
        let t = turns(270, Some(90), 270);
        assert_eq!((t.picture, t.controls), (180, 90));
    }

    /// Without the screen's turn, the hold stands in; without either, the
    /// layout's own turn (the picture unturned, as before).
    #[test]
    fn unknown_screen_turns_fall_back() {
        assert_eq!(turns(270, None, 90).picture, 180);
        assert_eq!(turns(270, None, 270).picture, 0);
        let t = turns(270, None, 0);
        assert_eq!((t.picture, t.controls), (0, 270));
        // A landscape window on a screen said to be upright (a desktop):
        // as unknown.
        assert_eq!(turns(270, Some(0), 90).picture, 180);
        assert_eq!(turns(270, Some(0), 0).picture, 0);
    }

    /// Turned 90° the portrait child's top edge runs down the landscape
    /// window's right edge, its bottom (the shutter) down the left;
    /// turned 270° the other way round.
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
