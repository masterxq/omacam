//! The zoom dial: a ruler of zoom values under a fixed marker, shown in
//! place of the zoom field when it is tapped or dragged. Values grow to the
//! right on a logarithmic scale, so dragging the ruler to the left zooms
//! in; the lens's limits and its 1× are marked and labeled, and a tap on a
//! mark goes there; the current value is written over the marker. Values
//! are written relative to a unit (the lens's own view), and the writing
//! turns with the phone.

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use gtk4 as gtk;

/// Pixels of drag per doubling of the zoom.
pub const PX_PER_DOUBLING: f64 = 120.0;

/// The zoom after dragging `dx` pixels from `base` (left zooms in).
pub fn zoom_after_drag(base: f64, dx: f64) -> f64 {
    base * 2f64.powf(-dx / PX_PER_DOUBLING)
}

/// Pull a zoom near one of `stops` onto it (a light detent): within 3 %.
pub fn snap(zoom: f64, stops: &[f64]) -> f64 {
    stops
        .iter()
        .copied()
        .find(|s| (zoom / s).ln().abs() < 0.03)
        .unwrap_or(zoom)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ZoomDial {
        pub value: Cell<f64>,
        pub range: Cell<(f64, f64)>,
        pub stops: RefCell<Vec<f64>>,
        /// What the written values are relative to.
        pub unit: Cell<f64>,
        /// Clockwise degrees the phone is turned (the writing turns back).
        pub turn: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ZoomDial {
        const NAME: &'static str = "OmacamZoomDial";
        type Type = super::ZoomDial;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("zoomdial");
        }
    }

    impl ObjectImpl for ZoomDial {}

    impl ZoomDial {
        pub fn unit(&self) -> f64 {
            let u = self.unit.get();
            if u > 0.0 {
                u
            } else {
                1.0
            }
        }
    }

    impl WidgetImpl for ZoomDial {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (120, 320, -1, -1),
                _ => (64, 64, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            let (lo, hi) = self.range.get();
            let value = self.value.get().clamp(lo, hi);
            if !(lo > 0.0 && hi >= lo && value > 0.0) {
                return;
            }
            let cx = w / 2.0;
            let x_of = |z: f64| cx + ((z.log2() - value.log2()) * PX_PER_DOUBLING) as f32;
            let white = |a: f32| gdk::RGBA::new(1.0, 1.0, 1.0, a);
            // Fine ticks every tenth of a doubling within the range.
            let base = (h - 10.0).max(0.0);
            let (lo2, hi2) = (lo.log2(), hi.log2());
            let mut t = (lo2 * 10.0).ceil() / 10.0;
            while t <= hi2 + 1e-9 {
                let x = x_of(2f64.powf(t));
                if x >= 0.0 && x <= w {
                    let fade = 1.0 - ((x - cx).abs() / cx).min(1.0) * 0.7;
                    snapshot.append_color(
                        &white(0.45 * fade),
                        &graphene::Rect::new(x - 0.5, base - 8.0, 1.0, 8.0),
                    );
                }
                t += 0.1;
            }
            // The stages: longer ticks, labeled.
            for &s in self
                .stops
                .borrow()
                .iter()
                .filter(|s| **s >= lo && **s <= hi)
            {
                let x = x_of(s);
                if x < -20.0 || x > w + 20.0 {
                    continue;
                }
                snapshot.append_color(
                    &white(0.9),
                    &graphene::Rect::new(x - 1.0, base - 14.0, 2.0, 14.0),
                );
                // At the marker the readout says it already.
                if (x - cx).abs() < 30.0 {
                    continue;
                }
                let layout =
                    widget.create_pango_layout(Some(&crate::app::stage_label(s / self.unit())));
                let (lw, lh) = layout.pixel_size();
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x, base - 32.0 + lh as f32 / 2.0));
                snapshot.rotate(-(self.turn.get() as f32));
                snapshot.translate(&graphene::Point::new(-lw as f32 / 2.0, -lh as f32 / 2.0));
                snapshot.append_layout(&layout, &white(0.85));
                snapshot.restore();
            }
            // The marker and the value over it, in the widget's CSS color
            // (the theme's accent).
            let accent = widget.color();
            snapshot.append_color(
                &accent,
                &graphene::Rect::new(cx - 1.5, base - 22.0, 3.0, 26.0),
            );
            let layout =
                widget.create_pango_layout(Some(&crate::app::format_zoom(value / self.unit())));
            let mut font = gtk::pango::FontDescription::new();
            font.set_weight(gtk::pango::Weight::Bold);
            font.set_size(13 * gtk::pango::SCALE);
            layout.set_font_description(Some(&font));
            let (lw, lh) = layout.pixel_size();
            // Turned a quarter, the readout stands as tall as it is wide.
            let turned = self.turn.get() % 180 == 90;
            let (bw, bh) = if turned {
                (lh as f32 + 4.0, lw as f32 + 16.0)
            } else {
                (lw as f32 + 16.0, lh as f32 + 4.0)
            };
            let (bx, by) = (cx - bw / 2.0, 0.0);
            let bounds = graphene::Rect::new(bx, by, bw, bh);
            snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(bounds, 10.0));
            snapshot.append_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.55), &bounds);
            snapshot.pop();
            snapshot.save();
            snapshot.translate(&graphene::Point::new(bx + bw / 2.0, by + bh / 2.0));
            snapshot.rotate(-(self.turn.get() as f32));
            snapshot.translate(&graphene::Point::new(-lw as f32 / 2.0, -lh as f32 / 2.0));
            snapshot.append_layout(&layout, &accent);
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct ZoomDial(ObjectSubclass<imp::ZoomDial>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ZoomDial {
    fn default() -> Self {
        Self::new()
    }
}

impl ZoomDial {
    pub fn new() -> ZoomDial {
        glib::Object::builder().property("hexpand", true).build()
    }

    /// The value, the range it can take, the values to mark, and what the
    /// written values are relative to.
    pub fn set(&self, value: f64, range: (f64, f64), stops: &[f64], unit: f64) {
        let imp = self.imp();
        let changed = imp.value.replace(value) != value
            || imp.range.replace(range) != range
            || imp.unit.replace(unit) != unit
            || *imp.stops.borrow() != stops;
        if changed {
            imp.stops.replace(stops.to_vec());
            self.queue_draw();
        }
    }

    /// How the phone is held, clockwise degrees: the writing turns back.
    pub fn set_turn(&self, degrees: u32) {
        if self.imp().turn.replace(degrees) != degrees {
            self.queue_draw();
        }
    }

    /// The mark at `x` (widget coordinates), if one is near.
    pub fn mark_at(&self, x: f64) -> Option<f64> {
        let imp = self.imp();
        let (lo, hi) = imp.range.get();
        let value = imp.value.get().clamp(lo, hi);
        let cx = self.width() as f64 / 2.0;
        mark_near(x, cx, value, &imp.stops.borrow())
    }
}

/// The value of the mark nearest to `x` within reach of a finger, on a
/// ruler whose marker at `cx` stands at `value`.
fn mark_near(x: f64, cx: f64, value: f64, stops: &[f64]) -> Option<f64> {
    const REACH: f64 = 28.0;
    stops
        .iter()
        .map(|&s| {
            (
                s,
                (cx + (s.log2() - value.log2()) * PX_PER_DOUBLING - x).abs(),
            )
        })
        .filter(|&(_, d)| d <= REACH)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, _)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_left_zooms_in_by_doublings() {
        assert_eq!(zoom_after_drag(1.0, 0.0), 1.0);
        assert!((zoom_after_drag(1.0, -PX_PER_DOUBLING) - 2.0).abs() < 1e-9);
        assert!((zoom_after_drag(2.0, PX_PER_DOUBLING) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_tap_near_a_mark_finds_it() {
        // At 2x in the middle of a 300 wide dial, 1x lies a doubling left.
        let stops = [1.0, 4.0];
        assert_eq!(
            mark_near(150.0 - PX_PER_DOUBLING + 10.0, 150.0, 2.0, &stops),
            Some(1.0)
        );
        assert_eq!(
            mark_near(150.0 + PX_PER_DOUBLING, 150.0, 2.0, &stops),
            Some(4.0)
        );
        assert_eq!(mark_near(150.0, 150.0, 2.0, &stops), None);
    }

    #[test]
    fn values_near_a_stage_snap_onto_it() {
        assert_eq!(snap(1.02, &[1.0, 2.0]), 1.0);
        assert_eq!(snap(1.97, &[1.0, 2.0]), 2.0);
        assert_eq!(snap(1.2, &[1.0, 2.0]), 1.2);
    }
}
