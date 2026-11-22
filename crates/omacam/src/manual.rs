//! The manual controls: one bar that the adjust button opens over the
//! bottom of the picture, with a chip per value (exposure correction,
//! focus, white balance, shutter time, ISO) and a histogram switch.
//!
//! Everything is automatic until the user sets a value: a tap on a chip
//! opens its slider, and moving the slider takes that one value over by
//! hand; the others stay automatic. A value set by hand shows in the
//! accent color with a dot beside its name, and the adjust button carries
//! the same dot while the bar is closed. "Auto" beside the slider gives
//! that value back to the camera. While a value is automatic its slider
//! follows what the camera chose (dimmed), so taking it over starts there.
//!
//! A chip whose control the camera doesn't offer is grayed out, with a
//! tooltip saying so.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use omacam_core::backend::{Awb, AwbPreset, Setting};
use omacam_core::controller::ExposureView;
use omacam_core::exposure_control::{format_shutter, iso, iso_stops, shutter_stops};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ev,
    Focus,
    WhiteBalance,
    Shutter,
    Iso,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Ev,
        Kind::Focus,
        Kind::WhiteBalance,
        Kind::Shutter,
        Kind::Iso,
    ];

    fn name(self) -> &'static str {
        match self {
            Kind::Ev => "Exposure",
            Kind::Focus => "Focus",
            Kind::WhiteBalance => "WB",
            Kind::Shutter => "Shutter",
            Kind::Iso => "ISO",
        }
    }

    /// Whether the camera offers the control.
    fn offered(self, view: &ExposureView) -> bool {
        let caps = &view.caps;
        match self {
            Kind::Ev => caps.ev.is_some(),
            Kind::Focus => caps.lens_position.is_some(),
            Kind::WhiteBalance => caps.white_balance(),
            Kind::Shutter => caps.manual_shutter(),
            Kind::Iso => caps.manual_gain(),
        }
    }

    /// Whether the user set the value by hand.
    pub fn is_set(self, view: &ExposureView) -> bool {
        match self {
            Kind::Ev => view.ev_user.abs() > 0.01,
            Kind::Focus => !view.pro.lens.is_auto(),
            Kind::WhiteBalance => !view.pro.white_balance.is_auto(),
            Kind::Shutter => !view.pro.shutter.is_auto(),
            Kind::Iso => !view.pro.gain.is_auto(),
        }
    }
}

/// What the user did in the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Exposure correction, EV (0 is automatic).
    Ev(f32),
    Shutter(Setting<i32>),
    Iso(Setting<u32>),
    WhiteBalance(Setting<Awb>),
    Focus(Setting<f32>),
    Histogram(bool),
    AllAuto,
}

const NOT_OFFERED: &str = "This camera doesn't offer it (yet)";

/// Exposure correction range shown, EV.
const EV_RANGE: f32 = 2.0;

/// The white balance presets from warm to cold, for a camera that takes
/// no color temperature.
const WB_ORDER: [AwbPreset; 4] = [
    AwbPreset::Incandescent,
    AwbPreset::Fluorescent,
    AwbPreset::Daylight,
    AwbPreset::Cloudy,
];

fn preset_name(p: AwbPreset) -> &'static str {
    match p {
        AwbPreset::Daylight => "Sun",
        AwbPreset::Cloudy => "Cloud",
        AwbPreset::Incandescent => "Bulb",
        AwbPreset::Fluorescent => "Tube",
    }
}

/// A lens position as words: a distance where the range is in dioptres
/// (libcamera's unit: 0 is infinity, 10 is 10 cm), else a share of the
/// travel from far to near.
pub fn format_focus(pos: f32, range: (f32, f32)) -> String {
    if pos <= range.0 + 0.005 {
        return "∞".into();
    }
    if range.1 > 1.5 {
        let m = 1.0 / pos.max(0.01);
        if m < 1.0 {
            format!("{:.0} cm", m * 100.0)
        } else {
            format!("{m:.1} m")
        }
    } else {
        let share = (pos - range.0) / (range.1 - range.0).max(1e-3);
        format!("{:.0} %", share * 100.0)
    }
}

fn format_ev(ev: f32) -> String {
    if ev.abs() < 0.05 {
        "±0".into()
    } else {
        format!("{ev:+.1}")
    }
}

fn format_wb(w: Awb) -> String {
    match w {
        Awb::Preset(p) => preset_name(p).into(),
        Awb::Kelvin(k) => format!("{k} K"),
        Awb::Gains(_) => "Manual".into(),
    }
}

/// The chip's text for a value: what was set by hand, or what auto chose.
fn value_text(kind: Kind, view: &ExposureView) -> String {
    let range = view.caps.lens_position.unwrap_or((0.0, 1.0));
    match kind {
        Kind::Ev => format_ev(view.ev_user),
        Kind::Focus => match view.pro.lens.manual() {
            Some(p) => format_focus(p, range),
            None => "Auto".into(),
        },
        Kind::WhiteBalance => match view.pro.white_balance.manual() {
            Some(w) => format_wb(w),
            None => "Auto".into(),
        },
        Kind::Shutter => match view.pro.shutter.manual() {
            Some(t) => format_shutter(t),
            None => view
                .shutter_us
                .map(format_shutter)
                .unwrap_or_else(|| "Auto".into()),
        },
        Kind::Iso => match view.pro.gain.manual() {
            Some(g) => iso(g as f64, view.base_iso).to_string(),
            None => view
                .iso
                .map(|i| i.to_string())
                .unwrap_or_else(|| "Auto".into()),
        },
    }
}

/// The values set by hand, as one short line ("Focus 25 cm · ISO 400"),
/// for the hint shown while the bar is closed; None when all are auto.
pub fn summary(view: &ExposureView) -> Option<String> {
    let parts: Vec<String> = Kind::ALL
        .iter()
        .filter(|k| k.offered(view) && k.is_set(view))
        .map(|k| format!("{} {}", k.name(), value_text(*k, view)))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Whether any value is set by hand.
pub fn any_set(view: &ExposureView) -> bool {
    Kind::ALL.iter().any(|k| k.offered(view) && k.is_set(view))
}

/// One slider: its positions and what they stand for.
enum Scale {
    /// Continuous between two values.
    Range(f64, f64),
    /// A list of values: the slider moves by whole steps.
    Steps(usize),
}

struct Chip {
    kind: Kind,
    button: gtk::Button,
    dot: gtk::Label,
    value: gtk::Label,
}

pub struct ManualBar {
    pub root: gtk::Box,
    chips: Vec<Chip>,
    histogram: gtk::ToggleButton,
    editor: gtk::Box,
    auto: gtk::Button,
    slider: gtk::Scale,
    slider_value: gtk::Label,
    start_icon: gtk::Image,
    end_icon: gtk::Image,
    selected: Cell<Option<Kind>>,
    /// The view the slider was last set from.
    view: RefCell<Option<ExposureView>>,
    /// The slider's own values: what each step stands for.
    shutters: RefCell<Vec<i32>>,
    isos: RefCell<Vec<u32>>,
    on_action: Rc<dyn Fn(Action)>,
    /// True while the bar sets its own slider: no action goes out.
    setting: Cell<bool>,
}

impl ManualBar {
    pub fn new(on_action: impl Fn(Action) + 'static) -> Rc<ManualBar> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.add_css_class("manual-bar");
        root.set_hexpand(true);

        let editor = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        editor.add_css_class("manual-editor");
        editor.set_visible(false);
        let auto = gtk::Button::with_label("Auto");
        auto.add_css_class("manual-auto");
        auto.set_tooltip_text(Some("Back to automatic"));
        auto.set_valign(gtk::Align::Center);
        let start_icon = gtk::Image::new();
        let end_icon = gtk::Image::new();
        for i in [&start_icon, &end_icon] {
            i.add_css_class("turnable");
            i.set_pixel_size(16);
        }
        let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        slider.set_hexpand(true);
        slider.set_draw_value(false);
        slider.add_css_class("manual-slider");
        let slider_value = gtk::Label::new(None);
        slider_value.add_css_class("manual-value");
        slider_value.add_css_class("turnable");
        slider_value.set_width_chars(6);
        editor.append(&auto);
        editor.append(&start_icon);
        editor.append(&slider);
        editor.append(&end_icon);
        editor.append(&slider_value);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.set_homogeneous(true);
        let mut chips = Vec::new();
        for kind in Kind::ALL {
            let name = gtk::Label::new(Some(kind.name()));
            name.add_css_class("chip-name");
            let dot = gtk::Label::new(Some("●"));
            dot.add_css_class("chip-dot");
            dot.set_visible(false);
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 3);
            top.set_halign(gtk::Align::Center);
            top.append(&dot);
            top.append(&name);
            let value = gtk::Label::new(Some("Auto"));
            value.add_css_class("chip-value");
            value.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
            b.append(&top);
            b.append(&value);
            let button = gtk::Button::new();
            button.set_child(Some(&b));
            button.add_css_class("manual-chip");
            button.add_css_class("turnable");
            row.append(&button);
            chips.push(Chip {
                kind,
                button,
                dot,
                value,
            });
        }
        let histogram = gtk::ToggleButton::new();
        histogram.set_icon_name("omacam-histogram-symbolic");
        histogram.set_tooltip_text(Some("Histogram"));
        histogram.add_css_class("manual-chip");
        histogram.add_css_class("turnable");
        row.append(&histogram);
        root.append(&editor);
        root.append(&row);

        let bar = Rc::new(ManualBar {
            root,
            chips,
            histogram,
            editor,
            auto,
            slider,
            slider_value,
            start_icon,
            end_icon,
            selected: Cell::new(None),
            view: RefCell::new(None),
            shutters: RefCell::new(Vec::new()),
            isos: RefCell::new(Vec::new()),
            on_action: Rc::new(on_action),
            setting: Cell::new(false),
        });
        for chip in &bar.chips {
            let (b, kind) = (Rc::downgrade(&bar), chip.kind);
            chip.button.connect_clicked(move |_| {
                if let Some(b) = b.upgrade() {
                    let now = (b.selected.get() != Some(kind)).then_some(kind);
                    b.select(now);
                }
            });
            // A long press gives the value back to auto.
            let press = gtk::GestureLongPress::new();
            let (b, kind) = (Rc::downgrade(&bar), chip.kind);
            press.connect_pressed(move |g, _, _| {
                g.set_state(gtk::EventSequenceState::Claimed);
                if let Some(b) = b.upgrade() {
                    b.to_auto(kind);
                }
            });
            chip.button.add_controller(press);
        }
        let b = Rc::downgrade(&bar);
        bar.histogram.connect_toggled(move |t| {
            if let Some(b) = b.upgrade() {
                (b.on_action)(Action::Histogram(t.is_active()));
            }
        });
        let b = Rc::downgrade(&bar);
        bar.auto.connect_clicked(move |_| {
            if let Some(b) = b.upgrade() {
                if let Some(kind) = b.selected.get() {
                    b.to_auto(kind);
                }
            }
        });
        let b = Rc::downgrade(&bar);
        bar.slider.connect_value_changed(move |s| {
            let Some(b) = b.upgrade() else { return };
            if !b.setting.get() {
                b.slider_moved(s.value());
            }
        });
        bar
    }

    /// Open one value's slider, or close it (None).
    pub fn select(&self, kind: Option<Kind>) {
        self.selected.set(kind);
        for c in &self.chips {
            if Some(c.kind) == kind {
                c.button.add_css_class("selected");
            } else {
                c.button.remove_css_class("selected");
            }
        }
        self.build_editor();
    }

    fn to_auto(&self, kind: Kind) {
        (self.on_action)(match kind {
            Kind::Ev => Action::Ev(0.0),
            Kind::Focus => Action::Focus(Setting::Auto),
            Kind::WhiteBalance => Action::WhiteBalance(Setting::Auto),
            Kind::Shutter => Action::Shutter(Setting::Auto),
            Kind::Iso => Action::Iso(Setting::Auto),
        });
    }

    /// Bring the chips and the open slider up to date.
    pub fn update(&self, view: &ExposureView) {
        for c in &self.chips {
            let offered = c.kind.offered(view);
            let set = offered && c.kind.is_set(view);
            c.button.set_sensitive(offered);
            c.button.set_tooltip_text((!offered).then_some(NOT_OFFERED));
            c.value.set_label(&if offered {
                value_text(c.kind, view)
            } else {
                "–".into()
            });
            c.dot.set_visible(set);
            if set {
                c.button.add_css_class("set");
            } else {
                c.button.remove_css_class("set");
            }
        }
        let rebuild = self
            .view
            .borrow()
            .as_ref()
            .is_none_or(|v| v.caps != view.caps || v.base_iso != view.base_iso);
        self.view.replace(Some(view.clone()));
        if rebuild {
            self.build_editor();
        } else {
            self.set_slider_from_view();
        }
    }

    /// The open value's slider: its range, end icons and position.
    fn build_editor(&self) {
        let Some(kind) = self.selected.get() else {
            self.editor.set_visible(false);
            return;
        };
        let Some(view) = self.view.borrow().clone() else {
            self.editor.set_visible(false);
            return;
        };
        if !kind.offered(&view) {
            self.editor.set_visible(false);
            return;
        }
        let caps = &view.caps;
        let scale = match kind {
            Kind::Ev => {
                let (lo, hi) = caps.ev.unwrap_or((-EV_RANGE, EV_RANGE));
                Scale::Range(lo.max(-EV_RANGE) as f64, hi.min(EV_RANGE) as f64)
            }
            Kind::Focus => {
                let (lo, hi) = caps.lens_position.unwrap_or((0.0, 1.0));
                Scale::Range(lo as f64, hi as f64)
            }
            Kind::WhiteBalance => match caps.color_temperature {
                Some((lo, hi)) => Scale::Range(lo as f64, hi as f64),
                None => Scale::Steps(WB_ORDER.len()),
            },
            Kind::Shutter => {
                let stops = caps.exposure_time.map(shutter_stops).unwrap_or_default();
                let n = stops.len();
                self.shutters.replace(stops);
                Scale::Steps(n)
            }
            Kind::Iso => {
                let stops = caps
                    .gain
                    .map(|g| iso_stops(view.base_iso, g))
                    .unwrap_or_default();
                let n = stops.len();
                self.isos.replace(stops);
                Scale::Steps(n)
            }
        };
        self.setting.set(true);
        self.slider.clear_marks();
        match scale {
            Scale::Range(lo, hi) => {
                self.slider.set_range(lo, hi.max(lo + 1e-3));
                let step = match kind {
                    Kind::Ev => 1.0 / 3.0,
                    Kind::WhiteBalance => 100.0,
                    _ => (hi - lo) / 200.0,
                };
                self.slider.set_increments(step, step * 3.0);
                self.slider
                    .set_round_digits(if kind == Kind::Ev { 2 } else { 3 });
            }
            Scale::Steps(n) => {
                self.slider.set_range(0.0, (n.max(2) - 1) as f64);
                self.slider.set_increments(1.0, 1.0);
                self.slider.set_round_digits(0);
            }
        }
        match kind {
            Kind::Ev => self.slider.add_mark(0.0, gtk::PositionType::Bottom, None),
            Kind::WhiteBalance if caps.color_temperature.is_some() => {
                for p in WB_ORDER {
                    self.slider
                        .add_mark(p.kelvin() as f64, gtk::PositionType::Bottom, None);
                }
            }
            _ => {}
        }
        let (start, end) = match kind {
            Kind::Focus => ("omacam-far-symbolic", "omacam-macro-symbolic"),
            Kind::Ev => ("", "omacam-exposure-symbolic"),
            _ => ("", ""),
        };
        self.start_icon.set_icon_name(Some(start));
        self.start_icon.set_visible(!start.is_empty());
        self.end_icon.set_icon_name(Some(end));
        self.end_icon.set_visible(!end.is_empty());
        self.setting.set(false);
        self.editor.set_visible(true);
        self.set_slider_from_view();
    }

    /// Put the slider where the value is: the one set by hand, or what
    /// auto chose (dimmed).
    fn set_slider_from_view(&self) {
        let (Some(kind), Some(view)) = (self.selected.get(), self.view.borrow().clone()) else {
            return;
        };
        let set = kind.is_set(&view);
        let range = view.caps.lens_position.unwrap_or((0.0, 1.0));
        let nearest = |len: usize, dist: &dyn Fn(usize) -> f64| {
            (0..len).min_by(|a, b| dist(*a).total_cmp(&dist(*b)))
        };
        let (pos, text) = match kind {
            Kind::Ev => (Some(view.ev_user as f64), format_ev(view.ev_user)),
            Kind::Focus => {
                let p = view.pro.lens.manual().or(view.lens_position);
                (
                    p.map(|p| p as f64),
                    p.map(|p| format_focus(p, range))
                        .unwrap_or_else(|| "Auto".into()),
                )
            }
            Kind::WhiteBalance => match (view.caps.color_temperature, view.pro.white_balance) {
                (Some(_), Setting::Manual(Awb::Kelvin(k))) => (Some(k as f64), format!("{k} K")),
                (Some(_), Setting::Manual(Awb::Preset(p))) => {
                    (Some(p.kelvin() as f64), preset_name(p).into())
                }
                (Some(_), _) => (Some(5500.0), "Auto".into()),
                (None, Setting::Manual(Awb::Preset(p))) => (
                    WB_ORDER.iter().position(|q| *q == p).map(|i| i as f64),
                    preset_name(p).into(),
                ),
                (None, _) => (Some(2.0), "Auto".into()),
            },
            Kind::Shutter => {
                let stops = self.shutters.borrow();
                let t = view.pro.shutter.manual().or(view.shutter_us);
                let i = t.and_then(|t| {
                    nearest(stops.len(), &|i| (stops[i] as f64 / t as f64).ln().abs())
                });
                (
                    i.map(|i| i as f64),
                    t.map(format_shutter).unwrap_or_else(|| "Auto".into()),
                )
            }
            Kind::Iso => {
                let stops = self.isos.borrow();
                let v = view
                    .pro
                    .gain
                    .manual()
                    .map(|g| iso(g as f64, view.base_iso))
                    .or(view.iso);
                let i = v.and_then(|v| {
                    nearest(stops.len(), &|i| (stops[i] as f64 / v as f64).ln().abs())
                });
                (
                    i.map(|i| i as f64),
                    v.map(|v| v.to_string()).unwrap_or_else(|| "Auto".into()),
                )
            }
        };
        self.setting.set(true);
        if let Some(p) = pos {
            self.slider.set_value(p);
        }
        self.setting.set(false);
        self.slider_value.set_label(&text);
        if set {
            self.slider.remove_css_class("auto");
            self.slider_value.add_css_class("set");
            self.auto.remove_css_class("active");
        } else {
            self.slider.add_css_class("auto");
            self.slider_value.remove_css_class("set");
            self.auto.add_css_class("active");
        }
    }

    /// The user moved the slider: that value is now set by hand.
    fn slider_moved(&self, v: f64) {
        let Some(kind) = self.selected.get() else {
            return;
        };
        let has_kelvin = self
            .view
            .borrow()
            .as_ref()
            .is_some_and(|v| v.caps.color_temperature.is_some());
        let step = v.round().max(0.0) as usize;
        let action = match kind {
            Kind::Ev => Action::Ev(((v * 3.0).round() / 3.0) as f32),
            Kind::Focus => Action::Focus(Setting::Manual(v as f32)),
            Kind::WhiteBalance if has_kelvin => Action::WhiteBalance(Setting::Manual(Awb::Kelvin(
                ((v / 100.0).round() * 100.0) as u32,
            ))),
            Kind::WhiteBalance => {
                let p = WB_ORDER[step.min(WB_ORDER.len() - 1)];
                Action::WhiteBalance(Setting::Manual(Awb::Preset(p)))
            }
            Kind::Shutter => match self.shutters.borrow().get(step) {
                Some(t) => Action::Shutter(Setting::Manual(*t)),
                None => return,
            },
            Kind::Iso => match self.isos.borrow().get(step) {
                Some(i) => Action::Iso(Setting::Manual(*i)),
                None => return,
            },
        };
        (self.on_action)(action);
    }

    /// Turn the histogram switch (the script's way in).
    pub fn set_histogram(&self, on: bool) {
        self.histogram.set_active(on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_reads_as_a_distance_or_a_share() {
        assert_eq!(format_focus(0.0, (0.0, 10.0)), "∞");
        assert_eq!(format_focus(4.0, (0.0, 10.0)), "25 cm");
        assert_eq!(format_focus(0.5, (0.0, 10.0)), "2.0 m");
        assert_eq!(format_focus(0.42, (0.0, 1.0)), "42 %");
    }

    #[test]
    fn the_summary_names_only_values_set_by_hand() {
        let mut view = ExposureView {
            caps: omacam_core::backend::Caps {
                lens_position: Some((0.0, 10.0)),
                exposure_time: Some((100, 66_666)),
                gain: Some((1.0, 16.0)),
                manual_exposure: true,
                ev: Some((-8.0, 8.0)),
                ..Default::default()
            },
            base_iso: 100.0,
            iso: Some(800),
            ..Default::default()
        };
        assert_eq!(summary(&view), None);
        assert!(!any_set(&view));
        view.pro.lens = Setting::Manual(4.0);
        view.pro.gain = Setting::Manual(4.0);
        assert_eq!(summary(&view).as_deref(), Some("Focus 25 cm · ISO 400"));
        view.ev_user = 0.7;
        assert!(summary(&view).unwrap().starts_with("Exposure +0.7"));
        assert!(any_set(&view));
    }
}
