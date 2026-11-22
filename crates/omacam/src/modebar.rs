//! The mode bar: one line of modes (Night, Photo, Video), each a symbol
//! that turns with the phone, the chosen one in the middle, highlighted
//! and, while the phone is upright, named beside its symbol (turned, the
//! words would lie on their side, and the bar has no room to turn them). A tap on a
//! name or a swipe across the bar chooses; the line slides so the chosen
//! mode is in the middle. Names never wrap: what doesn't fit runs off the
//! bar's edges.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;

/// How far a swipe must go to change the mode.
const SWIPE: f64 = 36.0;
/// How long the line takes to slide to a new mode.
const SLIDE: Duration = Duration::from_millis(200);

type OnSelect = Rc<dyn Fn(usize)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ModeBar {
        pub items: RefCell<Vec<gtk::Button>>,
        /// Each mode's name, shown beside the chosen one's symbol.
        pub words: RefCell<Vec<gtk::Label>>,
        /// The chosen mode is named (the phone is upright).
        pub named: Cell<bool>,
        pub active: Cell<usize>,
        /// The line's offset now (the middle of the chosen name sits at
        /// the bar's middle at 0), and where it slides from.
        pub offset: Cell<f64>,
        pub slide_from: Cell<f64>,
        pub slide_start: Cell<Option<i64>>,
        /// The finger's offset during a swipe.
        pub drag: Cell<f64>,
        pub on_select: RefCell<Option<OnSelect>>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ModeBar {
        const NAME: &'static str = "OmacamModeBar";
        type Type = super::ModeBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("modebar");
        }
    }

    impl ObjectImpl for ModeBar {
        fn dispose(&self) {
            for b in self.items.borrow_mut().drain(..) {
                b.unparent();
            }
        }
    }

    impl WidgetImpl for ModeBar {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let items = self.items.borrow();
            match orientation {
                gtk::Orientation::Horizontal => {
                    let nat: i32 = items
                        .iter()
                        .filter(|b| b.is_visible())
                        .map(|b| b.measure(orientation, -1).1)
                        .sum();
                    (0, nat, -1, -1)
                }
                _ => {
                    let h = items
                        .iter()
                        .map(|b| b.measure(orientation, -1).1)
                        .max()
                        .unwrap_or(0);
                    (h, h, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let items = self.items.borrow();
            let widths: Vec<i32> = items
                .iter()
                .map(|b| {
                    if b.is_visible() {
                        b.measure(gtk::Orientation::Horizontal, -1).1
                    } else {
                        0
                    }
                })
                .collect();
            let active = self.active.get().min(items.len().saturating_sub(1));
            let before: i32 = widths[..active].iter().sum();
            let center = before as f64 + widths.get(active).copied().unwrap_or(0) as f64 / 2.0;
            let mut x = width as f64 / 2.0 - center + self.offset.get() + self.drag.get();
            for (b, w) in items.iter().zip(&widths) {
                if b.is_visible() {
                    b.size_allocate(&gtk::Allocation::new(x.round() as i32, 0, *w, height), -1);
                }
                x += *w as f64;
            }
        }
    }
}

glib::wrapper! {
    pub struct ModeBar(ObjectSubclass<imp::ModeBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ModeBar {
    /// A bar of these modes (name, symbol), the first chosen.
    pub fn new(modes: &[(&str, &str)]) -> ModeBar {
        let bar: ModeBar = glib::Object::builder().property("hexpand", true).build();
        bar.set_overflow(gtk::Overflow::Hidden);
        bar.imp().named.set(true);
        let mut items = Vec::new();
        let mut words = Vec::new();
        for (i, (name, icon)) in modes.iter().enumerate() {
            let symbol = gtk::Image::from_icon_name(icon);
            symbol.add_css_class("turnable");
            let word = gtk::Label::new(Some(name));
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&symbol);
            row.append(&word);
            let b = gtk::Button::new();
            b.set_child(Some(&row));
            b.set_tooltip_text(Some(name));
            b.add_css_class("mode-item");
            words.push(word);
            b.set_parent(&bar);
            let weak = bar.downgrade();
            b.connect_clicked(move |_| {
                if let Some(bar) = weak.upgrade() {
                    bar.choose(i);
                }
            });
            items.push(b);
        }
        bar.imp().items.replace(items);
        bar.imp().words.replace(words);
        bar.mark();
        // Swipe: the line follows the finger; past `SWIPE` the next mode.
        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = bar.downgrade();
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(bar) = weak.upgrade() else { return };
            if bar.imp().drag.get() == 0.0 && dx.abs() < 8.0 {
                return;
            }
            if dy.abs() > dx.abs() * 1.5 && bar.imp().drag.get() == 0.0 {
                return;
            }
            g.set_state(gtk::EventSequenceState::Claimed);
            bar.imp().drag.set(dx);
            bar.queue_allocate();
        });
        let weak = bar.downgrade();
        drag.connect_drag_end(move |_, dx, _| {
            let Some(bar) = weak.upgrade() else { return };
            if bar.imp().drag.replace(0.0) == 0.0 {
                return;
            }
            // Keep where the finger left the line; the slide starts there.
            bar.imp().offset.set(bar.imp().offset.get() + dx);
            let step = if dx <= -SWIPE {
                bar.neighbor(1)
            } else if dx >= SWIPE {
                bar.neighbor(-1)
            } else {
                None
            };
            match step {
                Some(i) => bar.choose(i),
                None => bar.slide_home(),
            }
        });
        bar.add_controller(drag);
        bar
    }

    /// Called with the index of the mode the user chose.
    pub fn connect_selected(&self, f: impl Fn(usize) + 'static) {
        self.imp().on_select.replace(Some(Rc::new(f)));
    }

    pub fn selected(&self) -> usize {
        self.imp().active.get()
    }

    /// Choose a mode without telling the callback (the app's own changes).
    pub fn set_selected(&self, i: usize) {
        if self.imp().active.get() != i {
            self.move_to(i);
        }
    }

    /// Show or hide one mode (Night where the lens table allows bursts).
    pub fn set_item_visible(&self, i: usize, visible: bool) {
        if let Some(b) = self.imp().items.borrow().get(i) {
            b.set_visible(visible);
        }
        self.queue_resize();
    }

    /// The mode the user chose: slide to it and tell the callback.
    fn choose(&self, i: usize) {
        if i != self.imp().active.get() {
            self.move_to(i);
            let cb = self.imp().on_select.borrow().clone();
            if let Some(cb) = cb {
                cb(i);
            }
        } else {
            self.slide_home();
        }
    }

    /// The nearest visible mode `dir` steps to the right (or left).
    fn neighbor(&self, dir: i32) -> Option<usize> {
        let items = self.imp().items.borrow();
        let mut i = self.imp().active.get() as i32;
        loop {
            i += dir;
            if i < 0 || i as usize >= items.len() {
                return None;
            }
            if items[i as usize].is_visible() {
                return Some(i as usize);
            }
        }
    }

    fn move_to(&self, i: usize) {
        let imp = self.imp();
        // Keep the line where it is on screen: the old mode's middle is
        // now an offset from the new one's, which slides to zero.
        let widths: Vec<f64> = imp
            .items
            .borrow()
            .iter()
            .map(|b| {
                if b.is_visible() {
                    b.measure(gtk::Orientation::Horizontal, -1).1 as f64
                } else {
                    0.0
                }
            })
            .collect();
        let mid = |k: usize| widths[..k].iter().sum::<f64>() + widths.get(k).unwrap_or(&0.0) / 2.0;
        let old = imp.active.replace(i);
        if old < widths.len() && i < widths.len() {
            imp.offset.set(imp.offset.get() + mid(i) - mid(old));
        }
        self.mark();
        self.slide_home();
    }

    /// Name the chosen mode beside its symbol (the phone upright), or
    /// show the symbols alone (held otherwise).
    pub fn set_named(&self, named: bool) {
        if self.imp().named.replace(named) != named {
            self.mark();
        }
    }

    /// Highlight the chosen mode.
    fn mark(&self) {
        let active = self.imp().active.get();
        let named = self.imp().named.get();
        for (k, b) in self.imp().items.borrow().iter().enumerate() {
            if k == active {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
        for (k, w) in self.imp().words.borrow().iter().enumerate() {
            w.set_visible(named && k == active);
        }
        self.queue_resize();
    }

    /// Slide the line until the chosen mode is in the middle.
    fn slide_home(&self) {
        let imp = self.imp();
        imp.slide_from.set(imp.offset.get());
        imp.slide_start.set(None);
        if imp.tick.borrow().is_some() {
            return;
        }
        let id = self.add_tick_callback(|bar, clock| {
            let imp = bar.imp();
            let now = clock.frame_time();
            let start = *imp.slide_start.get().get_or_insert(now);
            imp.slide_start.set(Some(start));
            let t = ((now - start) as f64 / SLIDE.as_micros() as f64).min(1.0);
            let ease = 1.0 - (1.0 - t).powi(3);
            imp.offset.set(imp.slide_from.get() * (1.0 - ease));
            bar.queue_allocate();
            if t >= 1.0 {
                imp.offset.set(0.0);
                imp.tick.replace(None);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        imp.tick.replace(Some(id));
    }
}
