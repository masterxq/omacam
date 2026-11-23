//! The window: a full-screen viewfinder with the controls over its bottom.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use libadwaita as adw;

use omacam_core::backend::{Backend, Command, Event, Frame, FrameData, PixelLayout};
use omacam_core::controller::{Options, Output, Picture};
use omacam_core::flash::{FlashMode, Light};
use omacam_core::focus::Status as FocusStatus;
use omacam_core::lens::Role;
use omacam_core::orientation::{self, DeviceOrientation};
use omacam_core::pixels::Pixels;
use omacam_core::zoom::ZoomSource;
use omacam_core::{capture, Controller};

use crate::exif;
use crate::lensrow::{LensEvent, LensRow};
use crate::manual::ManualBar;
use crate::modebar::ModeBar;
use crate::photo;
use crate::settings::{Aspect, Settings, Timer};
use crate::viewfinder::{Decor, Insets, Placement, Ring, Viewfinder};
use crate::{CaptureMode, Cli};

const CSS: &str = r#"
window.omacam, window.omacam viewfinder { background: black; }

/* Over the top: the quick settings. */
.top-bar { padding: 6px 8px; background: rgba(0,0,0,0.35); }
.top-button {
  min-width: 40px; min-height: 40px; padding: 0 6px; border-radius: 999px;
  background: transparent; color: white; box-shadow: none;
  font-weight: bold; font-size: 13px;
}
.top-button:checked, .top-button.on { color: var(--accent-color); }
.flash-menu { background: rgba(48,48,48,0.94); border-radius: 18px; padding: 6px; }
.flash-choice {
  min-width: 64px; min-height: 54px; padding: 2px; border-radius: 14px;
  background: transparent; color: white; box-shadow: none; font-size: 11px;
}
.flash-choice.active { color: var(--accent-color); background: rgba(255,255,255,0.14); }

/* Over the bottom: the manual controls, the lens row, the modes and the
   shutter, half see-through where the picture runs under them. */
.bottom-panel {
  padding: 6px 12px 16px 12px;
  background: linear-gradient(to top, rgba(0,0,0,0.6), rgba(0,0,0,0.25));
}
.zoom-field {
  min-width: 52px; min-height: 44px; padding: 0 4px; border-radius: 14px;
  background: rgba(0,0,0,0.45); color: var(--accent-color); box-shadow: none;
  font-size: 15px; font-weight: bold; font-feature-settings: "tnum";
}
.lens-choice {
  min-width: 44px; min-height: 44px; padding: 0; border-radius: 999px;
  background: rgba(0,0,0,0.45); color: white; box-shadow: inset 0 0 0 2px rgba(255,255,255,0.85);
}
.lens-choice:disabled { opacity: 0.5; }
.side-button {
  min-width: 44px; min-height: 44px; padding: 0; border-radius: 999px;
  background: rgba(0,0,0,0.35); color: white; box-shadow: none;
}
.side-button:checked { color: var(--accent-color); background: rgba(255,255,255,0.18); }
.side-button.set { color: var(--accent-color); }
zoomdial { color: var(--accent-color); }
.zoom-dial { background: rgba(0,0,0,0.45); border-radius: 16px; }
modebar { min-height: 36px; }
.mode-item {
  min-height: 32px; padding: 0 14px; border-radius: 999px;
  background: transparent; color: rgba(255,255,255,0.75); box-shadow: none;
  font-weight: bold; font-size: 14px;
}
.mode-item.active { color: var(--accent-color); background: rgba(255,255,255,0.14); }
.manual-editor { background: rgba(0,0,0,0.6); border-radius: 16px; padding: 4px 10px; color: white; }
.manual-auto {
  min-height: 32px; padding: 0 10px; border-radius: 999px;
  background: rgba(255,255,255,0.12); color: white; box-shadow: none;
  font-size: 12px; font-weight: bold;
}
.manual-auto.active { background: var(--accent-bg-color); color: var(--accent-fg-color); }
scale.manual-slider { color: var(--accent-color); }
scale.manual-slider.auto { opacity: 0.5; }
.manual-value { color: white; font-weight: bold; font-feature-settings: "tnum"; }
.manual-value.set { color: var(--accent-color); }
.manual-chip {
  min-height: 44px; padding: 2px; border-radius: 12px;
  background: rgba(0,0,0,0.5); color: rgba(255,255,255,0.8); box-shadow: none;
}
.manual-chip.selected, .manual-chip:checked { background: rgba(255,255,255,0.22); }
.manual-chip.set .chip-value { color: var(--accent-color); }
.chip-name { font-size: 10px; opacity: 0.8; }
.chip-dot { font-size: 7px; color: var(--accent-color); }
.chip-value { font-size: 12px; font-weight: bold; font-feature-settings: "tnum"; }
.manual-hint {
  background: rgba(0,0,0,0.55); color: var(--accent-color); border-radius: 999px;
  padding: 0 2px 0 12px; font-size: 12px; font-weight: bold;
}
.manual-hint button {
  min-height: 28px; min-width: 28px; padding: 0; border-radius: 999px;
  background: transparent; box-shadow: none; color: white;
}

.shutter {
  min-width: 72px; min-height: 72px; padding: 0; margin: 0; border-radius: 999px;
  background: white;
  box-shadow: 0 0 0 5px rgba(255,255,255,0.35);
}
.shutter:active { background: #d8d8d8; }
.shutter.record { background: #e5383b; }
.shutter.recording { background: #e5383b; border-radius: 18px; }
.shutter.flash { box-shadow: 0 0 0 5px rgba(255,214,79,0.75); }
.rec-timer {
  background: #e5383b; color: white; font-weight: bold;
  border-radius: 999px; padding: 4px 12px; font-feature-settings: "tnum";
}
.shutter.busy { background: var(--accent-bg-color); }
.round-button {
  min-width: 56px; min-height: 56px; padding: 0; margin: 0; border-radius: 999px;
  background: rgba(255,255,255,0.16); color: white;
}

.zoom-indicator, .status {
  color: white; font-weight: bold; background: rgba(0,0,0,0.5);
  border-radius: 999px; padding: 6px 14px;
}
.zoom-indicator { font-size: 18px; }
.debug {
  font-family: monospace; font-size: 11px; color: #9f9;
  background: rgba(0,0,0,0.55); padding: 4px 6px; border-radius: 6px;
}
.lock-chip {
  background: var(--accent-bg-color); color: var(--accent-fg-color); font-weight: bold;
  border-radius: 999px; padding: 4px 12px;
}
scale.ev-slider { color: #ffd54f; }
.note-chip {
  background: rgba(0,0,0,0.6); color: #ffd54f; font-weight: bold; font-size: 12px;
  border-radius: 999px; padding: 4px 12px;
}

/* Held sideways, the controls turn in place (the layout stays portrait),
   as phone cameras do. */
.turnable { transition: transform 250ms ease-out; }
window.turn-90 .turnable, window.turn-90 toast { transform: rotate(-90deg); }
window.turn-270 .turnable, window.turn-270 toast { transform: rotate(90deg); }
window.turn-180 .turnable, window.turn-180 toast { transform: rotate(180deg); }

"#;

/// How long the EV slider stays after a tap or a drag.
const EV_SHOWN: Duration = Duration::from_secs(4);
/// How long the slider and the zoom readout stay after the last change.
const HIDE_AFTER: Duration = Duration::from_millis(1500);
/// How long the window may be out of sight (or, where the compositor
/// doesn't say, out of focus) before the cameras are let go: a look at the
/// photo just taken and back keeps the stream.
const UNFOCUSED_RELEASE: Duration = Duration::from_secs(10);

enum Msg {
    Event(Event),
    /// The newest frame is in the frame slot.
    FrameReady,
    Saved(Result<(PathBuf, photo::Bgrx), String>),
    /// A burst merged into one picture, ready to save.
    Merged(Result<Box<Picture>, String>),
    /// A command on the control socket (`omacam --remote shutter`).
    Remote(String),
    VideoSaved(Result<PathBuf, String>),
    /// GeoClue couldn't be had (location tags stay empty).
    NoLocation(String),
}

struct Fps {
    window_start: Instant,
    frames: u32,
    value: f64,
}

impl Fps {
    fn tick(&mut self) {
        self.frames += 1;
        let dt = self.window_start.elapsed();
        if dt >= Duration::from_secs(1) {
            self.value = self.frames as f64 / dt.as_secs_f64();
            self.frames = 0;
            self.window_start = Instant::now();
        }
    }
}

/// The modes of the mode bar, in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiMode {
    Night,
    Photo,
    Video,
}

const UI_MODES: [UiMode; 3] = [UiMode::Night, UiMode::Photo, UiMode::Video];

struct App {
    cli: Cli,
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    /// The layout, turned back when the shell turns the screen.
    turned: crate::turned::Turned,
    viewfinder: Viewfinder,
    /// The quick settings over the top, and the flash's choices under it.
    top_bar: gtk::Box,
    flash_button: gtk::Button,
    flash_menu: gtk::Revealer,
    flash_choices: Vec<(FlashMode, gtk::Button)>,
    timer_button: gtk::Button,
    aspect_button: gtk::Button,
    aspect_menu: gtk::Revealer,
    aspect_choices: Vec<(Aspect, gtk::Button)>,
    /// The lens row, the modes and the shutter row: the bar the picture
    /// sits above when it fits.
    bottom_fixed: gtk::Box,
    lens_row: Rc<LensRow>,
    /// The lens shown last (its name shows over the picture when it
    /// changes).
    shown_role: Cell<Option<Role>>,
    /// The window is in front (focused, not hidden).
    front: Cell<bool>,
    mode_bar: ModeBar,
    ui_mode: Cell<UiMode>,
    /// The manual controls, and the line naming the values set by hand
    /// while they are closed.
    manual: Rc<ManualBar>,
    manual_revealer: gtk::Revealer,
    manual_hint: gtk::Box,
    manual_hint_label: gtk::Label,
    zoom_indicator: gtk::Label,
    status: gtk::Label,
    status_box: gtk::Box,
    shutter: gtk::Button,
    flip: gtk::Button,
    thumb: gtk::Button,
    thumb_picture: gtk::Image,
    debug: gtk::Label,
    /// The flash mode before the torch was switched on, to go back to.
    before_torch: Cell<FlashMode>,

    backend: RefCell<Option<Box<dyn Backend>>>,
    controller: RefCell<Option<Controller>>,
    frame_slot: Arc<Mutex<Option<Frame>>>,
    tx: async_channel::Sender<Msg>,

    shown_camera: RefCell<Option<String>>,
    shown_size: Cell<(u32, u32)>,
    /// Mean brightness (0-255) and the AE values of the last shown frame,
    /// for the state log.
    shown_luma: Cell<Option<f64>>,
    /// When the viewfinder last took a frame (`--debug`: long gaps are
    /// logged, such as a photo's).
    shown_at: Cell<Option<Instant>>,
    shown_ae: Cell<(Option<i32>, Option<f32>, Option<f32>)>,
    fps: RefCell<Fps>,
    pinch_base: Cell<f64>,
    hide_source: RefCell<Option<glib::SourceId>>,
    unfocus_source: RefCell<Option<glib::SourceId>>,
    suspended: Cell<bool>,
    /// The compositor says when the window is out of sight (it has
    /// suspended it once): then only that lets the cameras go, not a loss
    /// of focus while the window still shows.
    reports_suspended: Cell<bool>,
    last_photo: RefCell<Option<PathBuf>>,
    last_error: RefCell<Option<(String, Instant)>>,
    /// The frames of the burst being taken (Night, HDR).
    burst: RefCell<Vec<Picture>>,
    lock_chip: gtk::Label,
    low_light_chip: gtk::Box,
    fps_toggle: gtk::ToggleButton,
    setting_fps: Cell<bool>,
    /// Photo: HDR bursts on or off.
    hdr_toggle: gtk::ToggleButton,
    setting_hdr: Cell<bool>,
    /// "Hold still" while a burst is taken, then "Merging" until it's saved.
    burst_chip: gtk::Box,
    burst_label: gtk::Label,
    /// The frames of the burst being merged (the shutter waits for it).
    merging: Cell<Option<usize>>,
    ev_slider: gtk::Scale,
    setting_ev: Cell<bool>,
    /// The EV slider shows until then (after a tap or a drag).
    ev_shown_until: Cell<Option<Instant>>,
    histogram: Cell<bool>,
    /// The last frame on screen, for metering a tap and the histogram.
    last_pixels: RefCell<Option<ShownPixels>>,
    settings: RefCell<Settings>,
    settings_path: PathBuf,
    /// The self-timer: its tick source and the seconds left.
    countdown: RefCell<Option<glib::SourceId>>,
    countdown_left: Cell<u32>,
    settings_dialog: RefCell<Option<adw::PreferencesDialog>>,
    /// The size, stride and layout of the frames on screen.
    shown_format: Cell<Option<(u32, u32, u32, PixelLayout)>>,
    gear: gtk::Button,
    rec_label: gtk::Label,
    recording: RefCell<Option<Recording>>,
    /// The recording's link to the camera thread.
    rec_tap: Arc<Mutex<Option<RecLink>>>,
    /// How the phone is held (the accelerometer).
    held: Rc<Cell<DeviceOrientation>>,
    sensor: RefCell<Option<crate::sensors::Accelerometer>>,
    /// The controls turned for a phone held this way (degrees clockwise).
    controls_turn: Cell<u32>,
    /// How the shell has turned the screen (Wayland).
    screen: Option<crate::screen::Screen>,
    /// The phone has a gyroscope stream (videos can be stabilized).
    has_gyro: bool,
    /// The lens table allows Night and HDR bursts ([`TableSpec::bursts`]):
    /// otherwise both stay hidden and photos are single ones.
    ///
    /// [`TableSpec::bursts`]: omacam_core::lens::TableSpec::bursts
    bursts: bool,
    /// Photos record their turn as the EXIF orientation
    /// ([`TableSpec::exif_orientation`]).
    ///
    /// [`TableSpec::exif_orientation`]: omacam_core::lens::TableSpec::exif_orientation
    exif_orientation: bool,
    /// The hardware JPEG encoder ([`TableSpec::jpeg_encoder`]).
    ///
    /// [`TableSpec::jpeg_encoder`]: omacam_core::lens::TableSpec::jpeg_encoder
    jpeg_encoder: Option<String>,
    /// GeoClue, while location tags are on.
    locator: RefCell<Option<crate::location::Locator>>,
    /// The flash LED, where the phone has one (`flash.rs`).
    flash_led: Option<crate::flash::FlashLed>,
}

/// A recording in progress.
struct Recording {
    recorder: omacam_record::Recorder,
    since: Instant,
    _gyro: Option<crate::sensors::gyro::Gyroscope>,
}

/// One back camera a recording can take its frames from: its frames'
/// geometry and the view of it that matches the others'.
#[derive(Debug, Clone, Copy)]
struct RecLens {
    /// Clockwise degrees that turn its frames upright.
    rotation: u32,
    mirror: bool,
    /// The share of its picture the preview mode covers, frame axes.
    coverage: (f64, f64),
    /// The view, frame axes, margin for stabilization included.
    view: omacam_record::View,
    /// Focal length over the upright picture's sides.
    focal: (f64, f64),
    /// The zoom crop (for how far stabilization may move it).
    scale: f64,
}

/// What the camera thread needs to feed a recording: the tap, the camera
/// it takes frames from, the cameras it may switch to, the stabilizer.
struct RecLink {
    tap: omacam_record::Tap,
    camera: String,
    lenses: std::collections::HashMap<String, RecLens>,
    /// The upright turn of the camera recording started on (the file's).
    base_rotation: u32,
    stabilizer: Option<Arc<Mutex<omacam_core::stabilize::Stabilizer>>>,
}

impl RecLink {
    /// Another camera started: its frames feed the recording from now
    /// on, when it's one the recording may switch to.
    fn camera_started(
        &mut self,
        camera: &str,
        size: omacam_core::Size,
        stride: u32,
        layout: PixelLayout,
    ) {
        if camera == self.camera {
            return;
        }
        let Some(lens) = self.lenses.get(camera).copied() else {
            return;
        };
        let input = omacam_record::Input {
            format: input_format(layout),
            width: size.width,
            height: size.height,
            stride,
            coverage: lens.coverage,
            flip: (lens.rotation + 360 - self.base_rotation) % 360 == 180,
        };
        match self.tap.set_input(input, lens.view) {
            Ok(()) => {
                tracing::info!(camera, "recording from another camera");
                self.camera = camera.to_string();
                if let Some(s) = &self.stabilizer {
                    s.lock().unwrap().reset_path();
                }
            }
            Err(e) => tracing::warn!(error = %e, "recording can't take this camera"),
        }
    }

    /// A frame of the recorded camera, moved against the shake.
    fn push(&self, frame: &Frame, pixels: omacam_core::backend::SharedPixels) {
        let mut shift = (0.0, 0.0);
        if let (Some(s), Some(lens)) = (&self.stabilizer, self.lenses.get(&self.camera)) {
            let up = s.lock().unwrap().correction(
                frame.meta.timestamp_ns as i64,
                frame.meta.exposure_us,
                lens.focal,
                lens.scale,
            );
            shift = omacam_core::framing::upright_to_frame_vector(up, lens.rotation, lens.mirror);
        }
        if let Err(e) = self.tap.push_shifted(pixels, shift) {
            tracing::warn!(error = %e, "frame not recorded");
        }
    }
}

/// Each camera's recorder view and crop scale now (margin included).
fn camera_views(app: &App) -> Option<Vec<(String, (omacam_record::View, f64))>> {
    let ids: Vec<String> = {
        let c = app.controller.borrow();
        c.as_ref()?
            .table()
            .lenses()
            .iter()
            .map(|l| l.id.clone())
            .collect()
    };
    let mut out = Vec::new();
    for id in ids {
        let t = app.controller.borrow().as_ref()?.transform(&id)?;
        let margin = app.video_margin(&id);
        out.push((id, (rec_view(&t, margin), t.scale * margin)));
    }
    Some(out)
}

/// A GeoClue fix as EXIF GPS tags, stamped with the fix's UTC time.
fn gps_of(f: crate::location::Fix) -> Option<exif::Gps> {
    let age = f.at.elapsed();
    let at = glib::DateTime::now_utc()
        .ok()?
        .add_seconds(-age.as_secs_f64())
        .ok()?;
    Some(exif::Gps {
        latitude: f.latitude,
        longitude: f.longitude,
        altitude: f.altitude,
        accuracy: f.accuracy.is_finite().then_some(f.accuracy),
        date: at.format("%Y:%m:%d").ok()?.to_string(),
        time: (at.hour() as u32, at.minute() as u32, at.seconds()),
    })
}

/// A lens's view for the recorder: the transform's crop on the frame's
/// own axes, tightened by `margin` (stabilization).
fn rec_view(t: &omacam_core::framing::Transform, margin: f64) -> omacam_record::View {
    omacam_record::View {
        scale: t.scale * margin,
        center: omacam_core::framing::upright_to_frame_point(t.center, t.rotation, t.mirror),
    }
}

fn input_format(layout: PixelLayout) -> omacam_record::InputFormat {
    let yuv = |c: omacam_core::backend::YuvColor| omacam_record::Yuv {
        bt709: c.bt709,
        limited: c.limited,
    };
    match layout {
        PixelLayout::Xrgb8888 => omacam_record::InputFormat::Bgrx,
        PixelLayout::Xbgr8888 => omacam_record::InputFormat::Rgbx,
        PixelLayout::Rgb888 => omacam_record::InputFormat::Bgr,
        PixelLayout::Bgr888 => omacam_record::InputFormat::Rgb,
        PixelLayout::Yuyv(c) => omacam_record::InputFormat::Yuy2(yuv(c)),
        PixelLayout::Nv12(c) => omacam_record::InputFormat::Nv12(yuv(c)),
    }
}

pub fn run(cli: Cli) -> glib::ExitCode {
    let app = adw::Application::builder()
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| build(app, cli.clone()));
    // Our options are parsed already; GTK gets none.
    app.run_with_args::<&str>(&[])
}

fn round_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("round-button");
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

fn top_button(icon: Option<&str>, label: Option<&str>, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    if let Some(i) = icon {
        row.append(&gtk::Image::from_icon_name(i));
    }
    let l = gtk::Label::new(label);
    l.set_visible(label.is_some());
    row.append(&l);
    b.set_child(Some(&row));
    b.add_css_class("top-button");
    b.add_css_class("turnable");
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

/// The label of a button made by [`top_button`].
fn top_button_label(b: &gtk::Button) -> Option<gtk::Label> {
    b.child()?.last_child()?.downcast::<gtk::Label>().ok()
}

/// The icon of a button made by [`top_button`].
fn top_button_icon(b: &gtk::Button) -> Option<gtk::Image> {
    b.child()?.first_child()?.downcast::<gtk::Image>().ok()
}

/// The flash menu's choices, in their order.
const FLASH_MENU: [FlashMode; 4] = [
    FlashMode::Auto,
    FlashMode::On,
    FlashMode::Off,
    FlashMode::Torch,
];

fn flash_icon(m: FlashMode) -> &'static str {
    match m {
        FlashMode::Auto => "omacam-flash-auto-symbolic",
        FlashMode::On => "omacam-flash-on-symbolic",
        FlashMode::Off => "omacam-flash-off-symbolic",
        FlashMode::Torch => "omacam-torch-symbolic",
    }
}

fn flash_label(m: FlashMode) -> &'static str {
    match m {
        FlashMode::Auto => "Auto",
        FlashMode::On => "On",
        FlashMode::Off => "Off",
        FlashMode::Torch => "Torch",
    }
}

fn aspect_label(a: Aspect) -> &'static str {
    match a {
        Aspect::Full => "Full",
        Aspect::FourThree => "4:3",
        Aspect::SixteenNine => "16:9",
        Aspect::Square => "1:1",
    }
}

fn build(gtk_app: &adw::Application, cli: Cli) {
    let display = gdk::Display::default().expect("a display");
    let monitor = crate::theme::follow_omarchy_theme();
    let css = gtk::CssProvider::new();
    css.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    crate::icons::register(&display);

    let window = adw::ApplicationWindow::builder()
        .application(gtk_app)
        .title("Camera")
        .default_width(cli.size.0)
        .default_height(cli.size.1)
        .build();
    window.add_css_class("omacam");
    // Keep the theme watcher alive with the window.
    if let Some(m) = monitor {
        unsafe_keep(&window, m);
    }

    // The picture takes the whole window; everything else lies over it,
    // and the picture sits between the bars where it fits.
    let viewfinder = Viewfinder::new();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&viewfinder));
    let settings_path = cli.settings.clone().unwrap_or_else(Settings::default_path);
    let settings = Settings::load(&settings_path);
    let table = cli.lens_table();
    let bursts = table.bursts();
    let exif_orientation = table.exif_orientation();
    let jpeg_encoder = table.jpeg_encoder().map(str::to_owned);

    // Top: the quick settings. Flash, timer, aspect ratio, HDR or 60 fps;
    // the settings gear at the end.
    let flash_button = top_button(Some(flash_icon(settings.flash)), None, "Flash");
    let timer_button = top_button(Some("omacam-timer-symbolic"), None, "Self-timer");
    let aspect_button = top_button(None, Some(aspect_label(settings.aspect)), "Aspect ratio");
    let hdr_toggle = gtk::ToggleButton::with_label("HDR");
    hdr_toggle.add_css_class("top-button");
    hdr_toggle.set_tooltip_text(Some("Merge a burst to keep highlights and shadows"));
    hdr_toggle.set_visible(false);
    // Video: 30 or 60 fps, where the lens's mode allows 60.
    let fps_toggle = gtk::ToggleButton::with_label("60 FPS");
    fps_toggle.add_css_class("top-button");
    fps_toggle.set_tooltip_text(Some("Record at 60 frames per second"));
    fps_toggle.set_visible(false);
    let gear = top_button(Some("omacam-settings-symbolic"), None, "Settings");
    let top_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    top_bar.add_css_class("top-bar");
    top_bar.set_valign(gtk::Align::Start);
    for w in [
        flash_button.upcast_ref::<gtk::Widget>(),
        timer_button.upcast_ref(),
        aspect_button.upcast_ref(),
        hdr_toggle.upcast_ref(),
        fps_toggle.upcast_ref(),
    ] {
        top_bar.append(w);
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top_bar.append(&spacer);
    top_bar.append(&gear);
    overlay.add_overlay(&top_bar);

    // The flash's four choices, under the top bar when its button is
    // tapped.
    let choices = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    choices.add_css_class("flash-menu");
    let mut flash_choices = Vec::new();
    for mode in FLASH_MENU {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        col.append(&gtk::Image::from_icon_name(flash_icon(mode)));
        col.append(&gtk::Label::new(Some(flash_label(mode))));
        let b = gtk::Button::new();
        b.set_child(Some(&col));
        b.add_css_class("flash-choice");
        b.add_css_class("turnable");
        choices.append(&b);
        flash_choices.push((mode, b));
    }
    let flash_menu = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&choices)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_start(8)
        .margin_top(56)
        .build();
    overlay.add_overlay(&flash_menu);
    // The aspect ratios, under the top bar when its button is tapped.
    let choices = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    choices.add_css_class("flash-menu");
    let mut aspect_choices = Vec::new();
    for a in Aspect::ALL {
        let b = gtk::Button::with_label(aspect_label(a));
        b.add_css_class("flash-choice");
        b.add_css_class("aspect-choice");
        b.add_css_class("turnable");
        choices.append(&b);
        aspect_choices.push((a, b));
    }
    let aspect_menu = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&choices)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_start(8)
        .margin_top(56)
        .build();
    overlay.add_overlay(&aspect_menu);

    let debug = gtk::Label::new(None);
    debug.add_css_class("debug");
    debug.set_xalign(0.0);
    debug.set_visible(cli.debug || settings.debug_overlay);
    debug.set_valign(gtk::Align::Start);
    debug.set_halign(gtk::Align::Start);
    debug.set_margin_top(60);
    debug.set_margin_start(12);
    debug.set_can_target(false);
    overlay.add_overlay(&debug);
    let rec_label = gtk::Label::new(None);
    rec_label.add_css_class("rec-timer");
    rec_label.set_halign(gtk::Align::Center);
    rec_label.set_valign(gtk::Align::Start);
    rec_label.set_visible(false);
    rec_label.set_can_target(false);
    overlay.add_overlay(&rec_label);

    // Center: zoom readout while zooming, and status text.
    let zoom_indicator = gtk::Label::new(None);
    zoom_indicator.add_css_class("zoom-indicator");
    zoom_indicator.set_halign(gtk::Align::Center);
    zoom_indicator.set_valign(gtk::Align::Center);
    zoom_indicator.set_visible(false);
    zoom_indicator.set_can_target(false);
    overlay.add_overlay(&zoom_indicator);
    let status = gtk::Label::new(Some("Starting the camera…"));
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    status_box.add_css_class("status");
    status_box.append(&spinner);
    status_box.append(&status);
    status_box.set_halign(gtk::Align::Center);
    status_box.set_valign(gtk::Align::Center);
    status_box.set_can_target(false);
    overlay.add_overlay(&status_box);

    // Over the top of the picture: the AE/AF lock, low light and burst
    // chips; beside the focus ring, the EV slider (placed when shown).
    let lock_chip = gtk::Label::new(Some("AE/AF lock"));
    lock_chip.add_css_class("lock-chip");
    lock_chip.set_visible(false);
    lock_chip.set_can_target(false);
    overlay.add_overlay(&lock_chip);
    // Low light: the main camera stands in for the telephoto.
    let low_light_chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    low_light_chip.add_css_class("note-chip");
    low_light_chip.append(&gtk::Image::from_icon_name("omacam-night-symbolic"));
    low_light_chip.append(&gtk::Label::new(Some("Low light: main camera")));
    low_light_chip.set_tooltip_text(Some(
        "Too dark for the telephoto: the main camera shows this zoom",
    ));
    low_light_chip.set_visible(false);
    low_light_chip.set_can_target(false);
    overlay.add_overlay(&low_light_chip);
    let flash_led = if cli.fake {
        None
    } else {
        crate::flash::FlashLed::find(cli.lens_table().flash())
    };
    let burst_label = gtk::Label::new(None);
    let burst_spinner = gtk::Spinner::new();
    burst_spinner.set_spinning(true);
    let burst_chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    burst_chip.add_css_class("note-chip");
    burst_chip.append(&burst_spinner);
    burst_chip.append(&burst_label);
    burst_chip.set_halign(gtk::Align::Center);
    burst_chip.set_valign(gtk::Align::Start);
    burst_chip.set_margin_top(64);
    burst_chip.set_visible(false);
    burst_chip.set_can_target(false);
    overlay.add_overlay(&burst_chip);
    let ev_slider = gtk::Scale::with_range(gtk::Orientation::Vertical, -2.0, 2.0, 0.1);
    ev_slider.set_inverted(true);
    ev_slider.set_draw_value(false);
    ev_slider.add_mark(0.0, gtk::PositionType::Right, None);
    ev_slider.add_css_class("ev-slider");
    ev_slider.set_size_request(-1, 170);
    ev_slider.set_halign(gtk::Align::Start);
    ev_slider.set_valign(gtk::Align::Start);
    ev_slider.set_visible(false);
    overlay.add_overlay(&ev_slider);

    // Bottom: the manual controls (when open) over the lens row, the mode
    // bar and the shutter row.
    let thumb_picture = gtk::Image::from_icon_name("omacam-gallery-symbolic");
    thumb_picture.set_pixel_size(22);
    let thumb = gtk::Button::new();
    thumb.set_child(Some(&thumb_picture));
    thumb.add_css_class("round-button");
    thumb.add_css_class("thumb");
    thumb.set_overflow(gtk::Overflow::Hidden);
    thumb.set_tooltip_text(Some("Last photo"));
    thumb.set_sensitive(false);
    let shutter = gtk::Button::new();
    shutter.add_css_class("shutter");
    shutter.set_tooltip_text(Some("Take a photo"));
    let flip = round_button("omacam-flip-symbolic", "Switch camera");
    // Fixed-size circles: never stretched by the row, whatever the window.
    for (b, size) in [(&thumb, 56), (&shutter, 72), (&flip, 56)] {
        b.set_size_request(size, size);
        b.set_halign(gtk::Align::Center);
        b.set_valign(gtk::Align::Center);
        b.set_hexpand(false);
        b.set_vexpand(false);
    }
    let row = gtk::CenterBox::new();
    row.set_start_widget(Some(&thumb));
    row.set_center_widget(Some(&shutter));
    row.set_end_widget(Some(&flip));
    row.set_vexpand(false);
    row.set_margin_start(12);
    row.set_margin_end(12);

    let mode_bar = ModeBar::new(&[
        ("Night", "omacam-night-symbolic"),
        ("Photo", "omacam-photo-symbolic"),
        ("Video", "omacam-video-symbolic"),
    ]);
    mode_bar.set_item_visible(0, bursts);
    mode_bar.set_selected(1);
    if omacam_record::init().is_err() {
        mode_bar.set_item_visible(2, false);
        tracing::warn!("GStreamer didn't start: no video mode");
    }

    let (ltx, lrx) = async_channel::unbounded::<LensEvent>();
    let lens_row = LensRow::new(move |e| {
        let _ = ltx.try_send(e);
    });
    let bottom_fixed = gtk::Box::new(gtk::Orientation::Vertical, 8);
    bottom_fixed.append(&lens_row.root);
    bottom_fixed.append(&mode_bar);
    bottom_fixed.append(&row);

    let (mtx, mrx) = async_channel::unbounded::<crate::manual::Action>();
    let manual = ManualBar::new(move |a| {
        let _ = mtx.try_send(a);
    });
    let manual_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .child(&manual.root)
        .build();
    let manual_hint_label = gtk::Label::new(None);
    manual_hint_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let hint_reset = gtk::Button::from_icon_name("window-close-symbolic");
    hint_reset.set_tooltip_text(Some("All back to automatic"));
    let manual_hint = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    manual_hint.add_css_class("manual-hint");
    manual_hint.append(&manual_hint_label);
    manual_hint.append(&hint_reset);
    manual_hint.set_halign(gtk::Align::Center);
    manual_hint.set_visible(false);

    let bottom_panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    bottom_panel.add_css_class("bottom-panel");
    bottom_panel.set_valign(gtk::Align::End);
    bottom_panel.append(&manual_revealer);
    bottom_panel.append(&manual_hint);
    bottom_panel.append(&lens_row.menu);
    bottom_panel.append(&bottom_fixed);
    overlay.add_overlay(&bottom_panel);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&overlay));
    // Portrait, whichever way the shell turns the screen.
    let turned = crate::turned::Turned::new(&toasts);
    turned.set_shutter_left(settings.shutter_left);
    window.set_content(Some(&turned));

    let (tx, rx) = async_channel::unbounded::<Msg>();
    let held = Rc::new(Cell::new(match cli.hold {
        Some(crate::Hold::Left) => DeviceOrientation::LeftUp,
        Some(crate::Hold::Right) => DeviceOrientation::RightUp,
        Some(crate::Hold::UpsideDown) => DeviceOrientation::BottomUp,
        _ => DeviceOrientation::Normal,
    }));
    let has_gyro = crate::sensors::gyro::available();
    tracing::info!(has_gyro, "gyroscope");
    let app = Rc::new(App {
        cli,
        window: window.clone(),
        toasts,
        turned,
        viewfinder,
        top_bar,
        flash_button,
        flash_menu,
        flash_choices,
        timer_button,
        aspect_button,
        aspect_menu,
        aspect_choices,
        bottom_fixed,
        lens_row,
        shown_role: Cell::new(None),
        front: Cell::new(true),
        mode_bar,
        ui_mode: Cell::new(UiMode::Photo),
        manual,
        manual_revealer,
        manual_hint,
        manual_hint_label,
        zoom_indicator,
        status,
        status_box,
        shutter,
        flip,
        thumb,
        thumb_picture,
        debug,
        before_torch: Cell::new(FlashMode::Auto),
        backend: RefCell::new(None),
        controller: RefCell::new(None),
        frame_slot: Arc::new(Mutex::new(None)),
        tx,
        shown_camera: RefCell::new(None),
        shown_size: Cell::new((0, 0)),
        shown_luma: Cell::new(None),
        shown_at: Cell::new(None),
        shown_ae: Cell::new((None, None, None)),
        fps: RefCell::new(Fps {
            window_start: Instant::now(),
            frames: 0,
            value: 0.0,
        }),
        pinch_base: Cell::new(1.0),
        hide_source: RefCell::new(None),
        unfocus_source: RefCell::new(None),
        suspended: Cell::new(false),
        reports_suspended: Cell::new(false),
        last_photo: RefCell::new(None),
        last_error: RefCell::new(None),
        burst: RefCell::new(Vec::new()),
        lock_chip,
        low_light_chip,
        fps_toggle: fps_toggle.clone(),
        setting_fps: Cell::new(false),
        hdr_toggle: hdr_toggle.clone(),
        setting_hdr: Cell::new(false),
        burst_chip,
        burst_label,
        merging: Cell::new(None),
        ev_slider,
        setting_ev: Cell::new(false),
        ev_shown_until: Cell::new(None),
        histogram: Cell::new(false),
        last_pixels: RefCell::new(None),
        settings: RefCell::new(settings),
        settings_path,
        countdown: RefCell::new(None),
        countdown_left: Cell::new(0),
        settings_dialog: RefCell::new(None),
        shown_format: Cell::new(None),
        gear: gear.clone(),
        rec_label,
        recording: RefCell::new(None),
        rec_tap: Arc::new(Mutex::new(None)),
        held: held.clone(),
        sensor: RefCell::new(None),
        controls_turn: Cell::new(0),
        screen: crate::screen::Screen::watch(&WidgetExt::display(&window)),
        has_gyro,
        bursts,
        exif_orientation,
        jpeg_encoder,
        locator: RefCell::new(None),
        flash_led,
    });
    // How the phone is held: the accelerometer, unless the command line
    // says (or the fake cameras run: the O key turns the phone then).
    if app.cli.hold.is_none() && !app.cli.fake {
        let a = Rc::downgrade(&app);
        let sensor = crate::sensors::Accelerometer::claim(held.clone(), move |_| {
            if let Some(a) = a.upgrade() {
                a.turn_controls();
            }
        });
        *app.sensor.borrow_mut() = sensor;
    }
    {
        let a = Rc::downgrade(&app);
        fps_toggle.connect_toggled(move |b| {
            let Some(a) = a.upgrade() else { return };
            if a.setting_fps.get() {
                return;
            }
            let fps = if b.is_active() { 60 } else { 30 };
            a.change_settings(|s| s.video_fps = fps);
            if let Some(out) = a.with_controller(|c| c.set_video_fps(fps)) {
                a.apply(out);
            }
        });
    }
    {
        let a = Rc::downgrade(&app);
        hdr_toggle.connect_toggled(move |b| {
            let Some(a) = a.upgrade() else { return };
            if a.setting_hdr.get() {
                return;
            }
            a.change_settings(|s| s.hdr = b.is_active());
            a.push_capture_mode();
        });
    }
    // Night or HDR from the command line: for this run only.
    match app.cli.capture {
        CaptureMode::Night | CaptureMode::Hdr if !app.bursts => {
            tracing::info!(
                capture = ?app.cli.capture,
                "no bursts on this camera (lens table): single photos instead"
            );
        }
        CaptureMode::Night => app.set_ui_mode(UiMode::Night),
        CaptureMode::Hdr => app.settings.borrow_mut().hdr = true,
        CaptureMode::Full | CaptureMode::Quick => {}
    }
    if !app.bursts && app.settings.borrow().hdr {
        tracing::info!("HDR is on in the settings, but this camera takes no bursts (lens table): single photos instead");
    }
    // The controls that turn with the phone.
    for w in [
        app.thumb.upcast_ref::<gtk::Widget>(),
        app.flip.upcast_ref(),
        app.lock_chip.upcast_ref(),
        app.low_light_chip.upcast_ref(),
        app.fps_toggle.upcast_ref(),
        app.hdr_toggle.upcast_ref(),
        app.burst_chip.upcast_ref(),
        app.zoom_indicator.upcast_ref(),
        app.status_box.upcast_ref(),
        app.rec_label.upcast_ref(),
    ] {
        w.add_css_class("turnable");
    }
    app.turn_controls();
    {
        let a = Rc::downgrade(&app);
        app.turned.connect_turn(move |_| {
            if let Some(a) = a.upgrade() {
                a.turn_controls();
            }
        });
        // The picture turns with the layout, in the same frame.
        let a = Rc::downgrade(&app);
        app.turned.connect_layout(move |layout| {
            if let Some(a) = a.upgrade() {
                a.viewfinder.set_flipped(a.turns_for(layout).picture == 180);
            }
        });
    }
    app.location_setting_changed();
    {
        let a = Rc::downgrade(&app);
        gear.connect_clicked(move |_| {
            if let Some(a) = a.upgrade() {
                a.open_settings();
            }
        });
    }
    {
        let a = Rc::downgrade(&app);
        hint_reset.connect_clicked(move |_| {
            if let Some(a) = a.upgrade() {
                a.manual_action(crate::manual::Action::AllAuto);
            }
        });
    }
    start_control_socket(app.tx.clone());

    app.start_backend();
    app.connect_input();

    let a = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            a.handle(msg);
        }
    });
    let a = Rc::downgrade(&app);
    glib::spawn_future_local(async move {
        while let Ok(e) = lrx.recv().await {
            let Some(a) = a.upgrade() else { break };
            a.lens_event(e);
        }
    });
    let a = Rc::downgrade(&app);
    glib::spawn_future_local(async move {
        while let Ok(action) = mrx.recv().await {
            let Some(a) = a.upgrade() else { break };
            a.manual_action(action);
        }
    });
    let a = Rc::downgrade(&app);
    glib::timeout_add_local(Duration::from_millis(30), move || {
        let Some(a) = a.upgrade() else {
            return glib::ControlFlow::Break;
        };
        a.tick();
        glib::ControlFlow::Continue
    });

    let a = app.clone();
    window.connect_close_request(move |_| {
        a.shutdown();
        glib::Propagation::Proceed
    });
    window.present();

    if let Some(text) = app.cli.script.clone() {
        let steps = crate::script::parse(&text).expect("checked in main");
        glib::spawn_future_local(run_script(app.clone(), steps));
    }
}

async fn run_script(app: Rc<App>, steps: Vec<crate::script::Step>) {
    use crate::script::Step;
    use omacam_core::backend::{Awb, Setting};
    for step in steps {
        tracing::info!(?step, "script");
        match step {
            Step::Wait(d) => glib::timeout_future(d).await,
            Step::Zoom(z) => app.zoom_to(z, ZoomSource::Preset),
            Step::Pinch(z) => {
                app.zoom_to(z, ZoomSource::Gesture);
                app.zoom_released();
            }
            Step::ZoomField => app.lens_row.zoom_field.emit_clicked(),
            Step::Mark(z) => {
                let unit = app
                    .controller
                    .borrow()
                    .as_ref()
                    .map_or(1.0, |c| c.view().zoom_unit);
                app.lens_event(LensEvent::Drag(z * unit));
                app.lens_event(LensEvent::Released);
            }
            Step::Dial(z) => {
                app.lens_row.show_dial();
                app.lens_event(LensEvent::Drag(z));
                app.lens_event(LensEvent::Released);
                app.lens_row.hide_dial_later();
            }
            Step::Flip => app.flip.emit_clicked(),
            Step::Macro => {
                let on = app
                    .controller
                    .borrow()
                    .as_ref()
                    .is_some_and(|c| c.view().macro_on);
                app.lens_row
                    .choose(if on { Role::Main } else { Role::Macro });
            }
            Step::Camera(role) => app.lens_row.choose(role),
            Step::LensMenu => app.lens_row.menu.set_reveal_child(true),
            Step::AspectMenu => app.open_menu(&app.aspect_menu, &app.aspect_button),
            Step::Shutter => app.shutter.emit_clicked(),
            Step::Shot(path) => match app.shot_after_paint(path.clone()).await {
                Ok(()) => tracing::info!(path = %path.display(), "window saved"),
                Err(e) => tracing::warn!(error = %e, "window not saved"),
            },
            Step::Settings(page) => {
                if app.settings_dialog.borrow().is_none() {
                    app.open_settings();
                }
                if let Some(d) = app.settings_dialog.borrow().as_ref() {
                    d.set_visible_page_name(&page);
                }
            }
            Step::CloseSettings => {
                let dialog = app.settings_dialog.borrow_mut().take();
                if let Some(d) = dialog {
                    d.close();
                }
            }
            Step::Mode(m) => match m.as_str() {
                "video" => app.set_ui_mode(UiMode::Video),
                "night" if !app.bursts => {
                    tracing::info!("no Night mode on this camera (lens table): photo mode instead");
                    app.set_ui_mode(UiMode::Photo);
                }
                "night" => app.set_ui_mode(UiMode::Night),
                _ => app.set_ui_mode(UiMode::Photo),
            },
            Step::Hdr(true) if !app.bursts => {
                tracing::info!("no HDR on this camera (lens table): single photos instead");
            }
            Step::Hdr(on) => app.hdr_toggle.set_active(on),
            Step::Tap(u, v) => app.tap_at(u, v),
            Step::Pro(on) => app.set_manual_open(on),
            Step::ProEdit(which) => {
                use crate::manual::Kind;
                let kind = match which.as_str() {
                    "ev" => Some(Kind::Ev),
                    "shutter" => Some(Kind::Shutter),
                    "iso" => Some(Kind::Iso),
                    "wb" => Some(Kind::WhiteBalance),
                    "focus" => Some(Kind::Focus),
                    _ => None,
                };
                app.manual.select(kind);
                app.refresh();
            }
            Step::ProShutter(us) => app.manual_action(crate::manual::Action::Shutter(if us == 0 {
                Setting::Auto
            } else {
                Setting::Manual(us as i32)
            })),
            Step::Focus(p) => app.manual_action(crate::manual::Action::Focus(match p {
                Some(p) => Setting::Manual(p as f32),
                None => Setting::Auto,
            })),
            Step::Iso(i) => app.manual_action(crate::manual::Action::Iso(match i {
                Some(i) => Setting::Manual(i),
                None => Setting::Auto,
            })),
            Step::Wb(k) => app.manual_action(crate::manual::Action::WhiteBalance(match k {
                Some(k) => Setting::Manual(Awb::Kelvin(k)),
                None => Setting::Auto,
            })),
            Step::Histogram(on) => app.manual.set_histogram(on),
            Step::Flash(mode) => app.set_flash(mode),
            Step::FlashMenu => app.open_menu(&app.flash_menu, &app.flash_button),
            Step::Aspect(a) => {
                let aspect = match a.as_str() {
                    "4:3" => Aspect::FourThree,
                    "16:9" => Aspect::SixteenNine,
                    "1:1" => Aspect::Square,
                    _ => Aspect::Full,
                };
                app.aspect_menu.set_reveal_child(false);
                app.change_settings(|s| s.aspect = aspect);
                app.refresh();
            }
            Step::Timer(t) => {
                let timer = match t {
                    3 => Timer::Three,
                    10 => Timer::Ten,
                    _ => Timer::Off,
                };
                app.change_settings(|s| s.timer = timer);
                app.refresh();
            }
            Step::Lock => {
                if let Some(out) = app.with_controller(|c| c.lock(Instant::now())) {
                    app.apply(out);
                }
            }
            Step::Ev(ev) => {
                app.ev_shown_until.set(Some(Instant::now() + EV_SHOWN));
                if let Some(out) = app.with_controller(|c| c.set_ev(ev as f32)) {
                    app.apply(out);
                }
            }
            Step::Hold(how) => {
                app.held.set(match how.as_str() {
                    "left" => DeviceOrientation::LeftUp,
                    "right" => DeviceOrientation::RightUp,
                    "upside-down" => DeviceOrientation::BottomUp,
                    _ => DeviceOrientation::Normal,
                });
                app.turn_controls();
            }
            Step::Quit => {
                app.window.close();
                return;
            }
        }
        app.log_state();
    }
}

/// Tie an object's lifetime to the window's.
fn unsafe_keep(window: &adw::ApplicationWindow, monitor: gio::FileMonitor) {
    let cell = RefCell::new(Some(monitor));
    window.connect_destroy(move |_| {
        cell.borrow_mut().take();
    });
}

/// The last shown frame's pixels: data, width, height, stride, layout.
type ShownPixels = (
    omacam_core::backend::SharedPixels,
    u32,
    u32,
    u32,
    PixelLayout,
);

fn memory_format(layout: PixelLayout) -> gdk::MemoryFormat {
    match layout {
        PixelLayout::Xrgb8888 => gdk::MemoryFormat::B8g8r8x8,
        PixelLayout::Xbgr8888 => gdk::MemoryFormat::R8g8b8x8,
        PixelLayout::Rgb888 => gdk::MemoryFormat::B8g8r8,
        PixelLayout::Bgr888 => gdk::MemoryFormat::R8g8b8,
        PixelLayout::Yuyv(_) => gdk::MemoryFormat::G8b8g8r8422,
        PixelLayout::Nv12(_) => gdk::MemoryFormat::G8B8r8420,
    }
}

/// The color state GTK turns a YUV frame into RGB with: the frame's matrix
/// and range, sRGB primaries and transfer (sYCC for BT.601 full range).
/// Built once per kind of frame.
fn yuv_color_state(c: omacam_core::backend::YuvColor) -> Option<gdk::ColorState> {
    thread_local! {
        static CACHE: RefCell<Vec<(omacam_core::backend::YuvColor, gdk::ColorState)>> =
            const { RefCell::new(Vec::new()) };
    }
    CACHE.with_borrow_mut(|cache| {
        if let Some((_, cs)) = cache.iter().find(|(k, _)| *k == c) {
            return Some(cs.clone());
        }
        let p = gdk::CicpParams::new();
        p.set_color_primaries(1);
        p.set_transfer_function(13);
        p.set_matrix_coefficients(if c.bt709 { 1 } else { 6 });
        p.set_range(if c.limited {
            gdk::CicpRange::Narrow
        } else {
            gdk::CicpRange::Full
        });
        match p.build_color_state() {
            Ok(cs) => {
                cache.push((c, cs.clone()));
                Some(cs)
            }
            Err(e) => {
                tracing::warn!(error = %e, "no color state for YUV frames");
                None
            }
        }
    })
}

/// A texture for the viewfinder, and for a memory frame its pixels as
/// shared bytes (the recorder takes the same bytes, no copy).
fn texture(display: &gdk::Display, frame: Frame) -> Option<(gdk::Texture, Option<glib::Bytes>)> {
    let (w, h) = (frame.size.width as i32, frame.size.height as i32);
    match frame.data {
        FrameData::Memory(_) | FrameData::Shared(_) => {
            let bytes = match frame.data {
                FrameData::Memory(v) => glib::Bytes::from_owned(v),
                FrameData::Shared(s) => glib::Bytes::from_owned(s),
                FrameData::Dmabuf { .. } => unreachable!(),
            };
            let tex = match frame.layout.yuv() {
                // YUV as it came from the camera: GTK turns it into RGB
                // on the GPU.
                Some(color) => {
                    let mut b = gdk::MemoryTextureBuilder::new()
                        .set_bytes(Some(&bytes))
                        .set_width(w)
                        .set_height(h)
                        .set_format(memory_format(frame.layout))
                        .set_stride(frame.stride as usize);
                    if let PixelLayout::Nv12(_) = frame.layout {
                        b = b
                            .set_stride_for_plane(1, frame.stride as usize)
                            .set_offset(1, frame.stride as usize * h as usize);
                    }
                    if let Some(cs) = yuv_color_state(color) {
                        b = b.set_color_state(&cs);
                    }
                    b.build()
                }
                None => gdk::MemoryTexture::new(
                    w,
                    h,
                    memory_format(frame.layout),
                    &bytes,
                    frame.stride as usize,
                )
                .upcast(),
            };
            Some((tex, Some(bytes)))
        }
        FrameData::Dmabuf {
            fd,
            offset,
            release,
        } => {
            // SAFETY: the fd belongs to the camera's buffer, which the
            // backend keeps alive and unqueued until `release` is dropped;
            // GTK calls the release function when it no longer reads it.
            let built = unsafe {
                gdk::DmabufTextureBuilder::new()
                    .set_display(display)
                    .set_width(w as u32)
                    .set_height(h as u32)
                    .set_fourcc(frame.layout.fourcc())
                    .set_modifier(0) // DRM_FORMAT_MOD_LINEAR
                    .set_n_planes(1)
                    .set_fd(0, fd)
                    .set_offset(0, offset)
                    .set_stride(0, frame.stride)
                    .build_with_release_func(move || drop(release))
            };
            match built {
                Ok(t) => Some((t, None)),
                Err(e) => {
                    tracing::warn!(error = %e, "dmabuf import failed");
                    None
                }
            }
        }
    }
}

/// Mean brightness (0-255) of a memory frame, from a sparse sample.
fn mean_luma(frame: &Frame) -> Option<f64> {
    let px = Pixels::new(
        frame.data.bytes()?,
        frame.size.width,
        frame.size.height,
        frame.stride,
        frame.layout,
    )?;
    let (mut sum, mut n) = (0.0, 0u32);
    for y in (0..px.height).step_by(16) {
        for x in (0..px.width).step_by(16) {
            sum += px.luma(x, y);
            n += 1;
        }
    }
    (n > 0).then(|| sum / n as f64)
}

pub(crate) fn format_zoom(z: f64) -> String {
    let r = (z * 10.0).round() / 10.0;
    if r < 1.0 {
        format!("{r:.1}×").trim_start_matches('0').to_string()
    } else if (r - r.round()).abs() < 0.05 {
        format!("{}×", r.round() as i64)
    } else {
        format!("{r:.1}×")
    }
}

/// A zoom stage's button label: ".5", "1", "2", "5".
pub(crate) fn stage_label(p: f64) -> String {
    if p < 1.0 {
        // One decimal: the full ultrawide (0.45x) reads ".5×", as on stock.
        format!("{:.1}×", (p * 10.0).round() / 10.0)
            .trim_start_matches('0')
            .to_string()
    } else {
        format!("{}×", p.round() as i64)
    }
}

impl App {
    fn start_backend(self: &Rc<Self>) {
        let tx = self.tx.clone();
        let slot = self.frame_slot.clone();
        let rec_tap = self.rec_tap.clone();
        let sink = Box::new(move |e: Event| match e {
            // A still from the stream beside the preview: to the
            // controller in order, never replaced by a newer preview
            // frame in the slot, never recorded.
            Event::Frame(f) if f.still => {
                let _ = tx.send_blocking(Msg::Event(Event::Frame(f)));
            }
            Event::Frame(mut f) => {
                // While recording, every frame of the recorded camera goes
                // to the encoder here, on the camera's thread, whatever
                // the viewfinder keeps up with; the same pixels, shared.
                if let FrameData::Memory(v) =
                    std::mem::replace(&mut f.data, FrameData::Memory(Vec::new()))
                {
                    let shared = omacam_core::backend::SharedPixels(Arc::new(v));
                    if let Some(link) = rec_tap.lock().unwrap().as_ref() {
                        if link.camera == f.camera {
                            link.push(&f, shared.clone());
                        }
                    }
                    f.data = FrameData::Shared(shared);
                }
                let was_empty = {
                    let mut s = slot.lock().unwrap();
                    let empty = s.is_none();
                    // An unshown older frame is dropped here (its buffer
                    // goes back to the camera).
                    *s = Some(f);
                    empty
                };
                if was_empty {
                    let _ = tx.send_blocking(Msg::FrameReady);
                }
            }
            other => {
                // A lens switch while recording: the recording takes the
                // new camera's frames from its first one, here on the
                // camera's thread, before any of them arrives.
                if let Event::Started {
                    camera,
                    size,
                    stride,
                    layout,
                    ..
                } = &other
                {
                    if let Some(link) = rec_tap.lock().unwrap().as_mut() {
                        link.camera_started(camera, *size, *stride, *layout);
                    }
                }
                let _ = tx.send_blocking(Msg::Event(other));
            }
        });
        match crate::spawn_backend(&self.cli, sink) {
            Ok(b) => *self.backend.borrow_mut() = Some(b),
            Err(e) => self.status.set_label(&format!("{e}")),
        }
    }

    fn apply(self: &Rc<Self>, outputs: Vec<Output>) {
        for o in outputs {
            match o {
                Output::Backend(cmd) => {
                    if matches!(
                        cmd,
                        Command::Start { .. } | Command::Stop { .. } | Command::Release
                    ) {
                        self.freeze_in_memory();
                    }
                    if let Some(b) = self.backend.borrow().as_ref() {
                        b.send(cmd);
                    }
                }
                Output::Show(frame) => self.show(frame),
                Output::Picture(p) => {
                    if self.settings.borrow().shutter_sound {
                        play_shutter_sound();
                    }
                    self.save(*p)
                }
                Output::BurstFrame(p) => {
                    let mut burst = self.burst.borrow_mut();
                    if burst.is_empty() && self.settings.borrow().shutter_sound {
                        play_shutter_sound();
                    }
                    burst.push(*p);
                }
                Output::BurstDone => self.merge_burst(),
                Output::Error(msg) => self.error(&msg),
                Output::Light(light) => {
                    if let Some(led) = &self.flash_led {
                        led.set(light);
                    }
                }
            }
        }
        self.refresh();
    }

    fn with_controller<R>(&self, f: impl FnOnce(&mut Controller) -> R) -> Option<R> {
        self.controller.borrow_mut().as_mut().map(f)
    }

    fn handle(self: &Rc<Self>, msg: Msg) {
        let now = Instant::now();
        match msg {
            Msg::Event(Event::Cameras(cameras)) => {
                let opts = Options {
                    capture_mode: self.capture_mode(),
                    transfer_exposure: self.cli.exposure_hints,
                    tap_to_focus: self.settings.borrow().tap_to_focus,
                    low_light: self.settings.borrow().low_light,
                    hold_lens: !self.settings.borrow().switch_lenses_by_zoom,
                    // The fake cameras show the controls; the light is
                    // only logged.
                    flash: self.flash_led.is_some() || self.cli.fake,
                    flash_mode: self.cli.flash.unwrap_or(self.settings.borrow().flash),
                    video_fps: self.settings.borrow().video_fps,
                    raw_merge: !self.cli.no_raw_merge,
                    ..Options::default()
                };
                // The list comes at startup and again whenever cameras come
                // or go (drivers load ~40 s into boot, or reload).
                let first = self.controller.borrow().is_none();
                let out = if first {
                    let mut c = Controller::new(&self.cli.lens_table(), &cameras, opts);
                    c.set_facing(self.cli.start_facing());
                    if let Some(z) = self.cli.zoom {
                        c.set_zoom(z, ZoomSource::Preset, now);
                    }
                    let mut out = c.set_foreground(self.front.get());
                    if !self.suspended.get() {
                        out.extend(c.resume(now));
                    }
                    *self.controller.borrow_mut() = Some(c);
                    out
                } else {
                    let spec = self.cli.lens_table();
                    self.with_controller(|c| c.set_cameras(&spec, &cameras, now))
                        .unwrap_or_default()
                };
                let has = self
                    .controller
                    .borrow()
                    .as_ref()
                    .is_some_and(|c| c.has_cameras());
                if has {
                    tracing::info!(count = cameras.len(), "cameras");
                    self.status.set_label("Starting the camera…");
                } else {
                    tracing::info!("no cameras yet; waiting");
                    self.status.set_label("Waiting for cameras…");
                    self.viewfinder.clear();
                    self.shown_camera.borrow_mut().take();
                }
                if !has || self.shown_camera.borrow().is_none() {
                    self.status_box.set_visible(true);
                }
                self.apply(out);
            }
            Msg::Event(e) => {
                let out = self.with_controller(|c| c.on_event(e, now));
                if let Some(out) = out {
                    self.apply(out);
                }
            }
            Msg::FrameReady => {
                let frame = self.frame_slot.lock().unwrap().take();
                if let Some(f) = frame {
                    let out = self.with_controller(|c| c.on_event(Event::Frame(f), now));
                    if let Some(out) = out {
                        self.apply(out);
                    }
                }
            }
            Msg::Saved(Ok((path, thumb))) => {
                let tex = gdk::MemoryTexture::new(
                    thumb.width as i32,
                    thumb.height as i32,
                    gdk::MemoryFormat::B8g8r8x8,
                    &glib::Bytes::from_owned(thumb.data),
                    thumb.width as usize * 4,
                );
                self.thumb_picture.set_paintable(Some(&tex));
                self.thumb_picture.set_pixel_size(56);
                self.thumb.set_sensitive(true);
                tracing::info!(path = %path.display(), "photo saved");
                *self.last_photo.borrow_mut() = Some(path);
            }
            Msg::Saved(Err(e)) => self.error(&format!("Photo not saved: {e}")),
            Msg::Merged(result) => {
                self.merging.set(None);
                match result {
                    Ok(p) => self.save(*p),
                    Err(e) => self.error(&format!("Photo not saved: {e}")),
                }
                self.refresh();
            }
            Msg::VideoSaved(Ok(path)) => {
                self.thumb.set_sensitive(true);
                *self.last_photo.borrow_mut() = Some(path);
            }
            Msg::VideoSaved(Err(e)) => self.error(&format!("Video not saved: {e}")),
            Msg::NoLocation(why) => {
                tracing::info!(why, "location tags without a location");
                // Tried again when the setting is next changed.
                self.locator.borrow_mut().take();
                self.error("No location for tags: turn on Location in the quick settings");
            }
            Msg::Remote(cmd) => match cmd.as_str() {
                "shutter" => self.press_shutter(),
                other => tracing::warn!(command = other, "unknown remote command"),
            },
        }
    }

    fn show(self: &Rc<Self>, frame: Frame) {
        let placement = self.placement_for(&frame.camera);
        let camera = frame.camera.clone();
        let size = (frame.size.width, frame.size.height);
        if self.cli.debug || self.cli.script.is_some() {
            self.shown_luma.set(mean_luma(&frame));
            let now = Instant::now();
            if let Some(gap) = self.shown_at.replace(Some(now)).map(|t| now - t) {
                if gap >= Duration::from_millis(150) {
                    tracing::info!(ms = gap.as_millis() as u64, "viewfinder waited for a frame");
                }
            }
        }
        // The exposure the camera chose, for the log.
        self.shown_ae.set((
            frame.meta.exposure_us,
            frame.meta.analog_gain,
            frame.meta.digital_gain,
        ));
        if let FrameData::Shared(px) = &frame.data {
            *self.last_pixels.borrow_mut() = Some((
                px.clone(),
                frame.size.width,
                frame.size.height,
                frame.stride,
                frame.layout,
            ));
        }
        let format = (
            frame.size.width,
            frame.size.height,
            frame.stride,
            frame.layout,
        );
        if let Some((tex, _)) = texture(&WidgetExt::display(&self.window), frame) {
            self.shown_format.set(Some(format));
            // Another camera's first frame: the old one fades out over it.
            let switched = self
                .shown_camera
                .borrow()
                .as_ref()
                .is_some_and(|c| *c != camera);
            if switched {
                self.viewfinder.switch_to(tex, placement);
            } else {
                self.viewfinder.set_frame(tex, placement);
            }
            self.status_box.set_visible(false);
            *self.shown_camera.borrow_mut() = Some(camera);
            self.shown_size.set(size);
            self.fps.borrow_mut().tick();
        }
    }

    /// Before a stream stops: keep the frame on screen as a memory copy,
    /// so the camera's dmabuf goes back before its buffers are freed.
    fn freeze_in_memory(&self) {
        let Some(tex) = self.viewfinder.texture() else {
            return;
        };
        if !tex.is::<gdk::DmabufTexture>() {
            return;
        }
        let mut downloader = gdk::TextureDownloader::new(&tex);
        downloader.set_format(gdk::MemoryFormat::B8g8r8a8);
        let (bytes, stride) = downloader.download_bytes();
        let copy = gdk::MemoryTexture::new(
            tex.width(),
            tex.height(),
            gdk::MemoryFormat::B8g8r8a8,
            &bytes,
            stride,
        );
        self.viewfinder
            .set_frame(copy.upcast(), self.viewfinder.placement());
    }

    fn placement_for(&self, camera: &str) -> Placement {
        self.controller
            .borrow()
            .as_ref()
            .and_then(|c| c.transform(camera))
            .map(|t| Placement {
                scale: t.scale * self.video_margin(camera),
                rotation: t.rotation,
                mirror: t.mirror,
                center: t.center,
                coverage: t.coverage,
            })
            .unwrap_or_default()
    }

    fn save(self: &Rc<Self>, p: Picture) {
        // A quick flash, so the press visibly did something.
        self.viewfinder.set_opacity(0.35);
        let vf = self.viewfinder.clone();
        glib::timeout_add_local_once(Duration::from_millis(120), move || vf.set_opacity(1.0));

        let settings = self.settings.borrow().clone();
        let dir = self
            .cli
            .output_dir
            .clone()
            .unwrap_or_else(|| settings.photo_dir());
        let now = glib::DateTime::now_local().ok();
        let fmt = |f: &str| {
            now.as_ref()
                .and_then(|d| d.format(f).ok())
                .map(|s| s.to_string())
        };
        let stamp = fmt("%Y%m%d_%H%M%S").unwrap_or_else(|| "photo".into());
        let (make, model) = exif::device();
        let lens_make = make.clone();
        let held = self.held.get();
        let front = p.lens.role == Role::Front;
        // Held sideways: the picture turns with the phone, and the aspect
        // ratio is a landscape one.
        let turn = orientation::turn(p.rotation, held, front);
        let aspect =
            settings
                .aspect
                .portrait_ratio()
                .map(|a| if held.is_landscape() { 1.0 / a } else { a });
        let subsec = now
            .as_ref()
            .map(|d| format!("{:03}", d.microsecond() / 1000));
        let lens_model = Some(format!(
            "{}{} camera{}{}",
            model
                .as_deref()
                .map(|m| format!("{m} "))
                .unwrap_or_default(),
            p.lens.name,
            p.lens
                .focal_length_mm
                .map(|f| format!(" {f:.2}mm"))
                .unwrap_or_default(),
            p.lens
                .f_number
                .map(|n| format!(" f/{n}"))
                .unwrap_or_default(),
        ));
        let opts = PhotoOptions {
            turn,
            quality: settings.jpeg_quality as i32,
            aspect,
            portrait_aspect: settings.aspect.portrait_ratio(),
            medium: settings.resolution == crate::settings::Resolution::Medium,
            mirror: settings.mirror_front && p.lens.role == Role::Front,
            exif_orientation: self.exif_orientation,
            jpeg_encoder: self.jpeg_encoder.clone(),
            exif: exif::Info {
                make,
                model,
                software: format!("omacam {}", env!("CARGO_PKG_VERSION")),
                datetime: fmt("%Y:%m:%d %H:%M:%S").unwrap_or_default(),
                offset: fmt("%:z"),
                lens_make,
                lens_model,
                subsec,
                // Location tags: off by default (Q5); the newest GeoClue
                // fix when on.
                gps: self.location_fix().and_then(gps_of),
                ..exif::Info::default()
            },
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = encode_and_write(&p, &dir, &stamp, opts).map_err(|e| format!("{e:#}"));
            let _ = tx.send_blocking(Msg::Saved(result));
        });
    }

    /// Merge the burst's frames off the UI thread; the result comes back as
    /// [`Msg::Merged`] and is saved like any picture.
    fn merge_burst(self: &Rc<Self>) {
        let frames = std::mem::take(&mut *self.burst.borrow_mut());
        // The burst was taken in the mode still selected: the mode switch
        // is off while the camera takes it.
        let hdr = self.ui_mode.get() != UiMode::Night;
        self.merging.set(Some(frames.len()));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = photo::merge_burst(frames, hdr)
                .map(Box::new)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send_blocking(Msg::Merged(result));
        });
    }

    /// The photo capture the controls ask for: a Night or HDR burst, or the
    /// command line's single frame. No burst where the lens table has none.
    fn capture_mode(&self) -> capture::Mode {
        if self.bursts && self.ui_mode.get() == UiMode::Night {
            capture::Mode::Burst(capture::BurstSpec::NIGHT)
        } else if self.bursts && self.settings.borrow().hdr {
            capture::Mode::Burst(capture::BurstSpec::HDR)
        } else if self.cli.capture == CaptureMode::Quick {
            capture::Mode::Quick
        } else {
            capture::Mode::Full
        }
    }

    fn push_capture_mode(self: &Rc<Self>) {
        let mode = self.capture_mode();
        self.with_controller(|c| c.set_capture_mode(mode));
        self.refresh();
    }

    fn error(&self, msg: &str) {
        tracing::warn!("{msg}");
        let mut last = self.last_error.borrow_mut();
        let repeat = last
            .as_ref()
            .is_some_and(|(m, t)| m == msg && t.elapsed() < Duration::from_secs(10));
        if !repeat {
            // Errors stay 10 s (or until dismissed): they often come while
            // the user looks at the picture, not at the toast.
            let toast = adw::Toast::new(msg);
            toast.set_timeout(10);
            toast.set_priority(adw::ToastPriority::High);
            self.toasts.add_toast(toast);
            *last = Some((msg.to_string(), Instant::now()));
        }
    }

    /// Bring the controls in line with the controller.
    fn refresh(self: &Rc<Self>) {
        let Some(view) = self.controller.borrow().as_ref().map(|c| c.view()) else {
            return;
        };
        let video = view.mode == omacam_core::controller::Mode::Video;
        let busy = view.capturing || self.merging.get().is_some();
        self.lens_row
            .update(&view, !view.capturing, !view.capturing);
        // A new lens: its name over the picture for a moment.
        if let Some(role) = view.lens_role {
            let before = self.shown_role.replace(Some(role));
            if before.is_some_and(|b| b != role) {
                self.flash_note(crate::lensrow::lens_name(role));
            }
        }
        self.flip.set_visible(view.can_flip);
        self.flip.set_sensitive(!view.capturing && !view.recording);
        if view.capturing {
            self.shutter.add_css_class("busy");
        } else {
            self.shutter.remove_css_class("busy");
        }
        self.shutter
            .set_sensitive(!busy && (!view.switching || view.recording));
        let night = self.ui_mode.get() == UiMode::Night;
        if video {
            self.shutter.add_css_class("record");
            self.shutter.set_tooltip_text(Some(if view.recording {
                "Stop recording"
            } else {
                "Record"
            }));
        } else {
            self.shutter.remove_css_class("record");
            self.shutter.set_tooltip_text(Some(if night {
                "Take a Night photo"
            } else {
                "Take a photo"
            }));
        }
        if view.recording {
            self.shutter.add_css_class("recording");
        } else {
            self.shutter.remove_css_class("recording");
        }
        self.mode_bar
            .set_sensitive(!view.recording && !view.capturing);
        // The controller's mode wins (a recording or a photo may have
        // held a change back).
        let shown = self.ui_mode.get();
        if (shown == UiMode::Video) != video {
            let m = if video { UiMode::Video } else { UiMode::Photo };
            self.ui_mode.set(m);
            self.mode_bar
                .set_selected(UI_MODES.iter().position(|x| *x == m).unwrap_or(1));
        }
        self.gear.set_sensitive(!view.recording);
        if let Some(r) = self.recording.borrow().as_ref() {
            // Every lens's view follows the zoom; the recorded camera's
            // goes to the recorder, the others' wait for a switch.
            let camera = self
                .rec_tap
                .lock()
                .unwrap()
                .as_ref()
                .map(|l| l.camera.clone());
            if let (Some(camera), Some(lenses)) = (camera, camera_views(self)) {
                if let Some(link) = self.rec_tap.lock().unwrap().as_mut() {
                    for (id, (view, scale)) in lenses {
                        if let Some(l) = link.lenses.get_mut(&id) {
                            l.view = view;
                            l.scale = scale;
                        }
                    }
                    if let Some(l) = link.lenses.get(&camera) {
                        link.tap.set_view(l.view);
                    }
                }
            }
            self.rec_label.set_label(&format!(
                "● {}",
                omacam_record::format_elapsed(r.since.elapsed())
            ));
            self.rec_label.set_visible(true);
        }
        // Zoom a frozen frame with the fingers while a lens starts.
        if let Some(cam) = self.shown_camera.borrow().as_ref() {
            self.viewfinder.set_placement(self.placement_for(cam));
        }
        let settings = self.settings.borrow().clone();
        // The focus mark while it scans, a little smaller once sharp;
        // afterwards a small, fainter one where focus and metering still
        // look, until given up. Light corners with a dark edge, so they
        // show on any picture and leave the subject free.
        let white = |a| gdk::RGBA::new(1.0, 1.0, 1.0, a);
        let ring = view
            .focus
            .map(|f| {
                let (half, color) = match f.status {
                    FocusStatus::Scanning => (36.0, white(0.95)),
                    FocusStatus::Focused | FocusStatus::Following => (30.0, white(0.95)),
                    FocusStatus::Failed => (36.0, gdk::RGBA::new(1.0, 0.55, 0.5, 0.95)),
                    FocusStatus::FixedFocus | FocusStatus::Unavailable => (36.0, white(0.5)),
                };
                Ring {
                    u: f.u,
                    v: f.v,
                    half,
                    color,
                }
            })
            .or_else(|| {
                view.tap_point.map(|(u, v)| Ring {
                    u,
                    v,
                    half: 16.0,
                    color: white(0.6),
                })
            });
        // Video records 16:9, upright 9:16: show what goes in the file.
        let aspect = if video {
            Some(9.0 / 16.0)
        } else {
            settings.aspect.portrait_ratio()
        };
        let histogram = if self.histogram.get() {
            self.last_pixels
                .borrow()
                .as_ref()
                .and_then(|(px, w, h, stride, layout)| {
                    Pixels::new(px.as_ref(), *w, *h, *stride, *layout)
                })
                .map(|px| luma_histogram(&px, 32))
        } else {
            None
        };
        self.viewfinder.set_decor(Decor {
            aspect,
            grid: settings.grid.lines(),
            histogram,
            ring,
        });
        let focus_note = match view.focus.map(|f| f.status) {
            Some(FocusStatus::FixedFocus) => Some("Fixed focus"),
            Some(FocusStatus::Unavailable) => Some("Focus unavailable"),
            _ => None,
        };
        if let Some(note) = focus_note {
            self.zoom_indicator.set_label(note);
            self.zoom_indicator.set_visible(true);
        } else if self.countdown.borrow().is_none() && self.hide_source.borrow().is_none() {
            self.zoom_indicator.set_visible(false);
        }
        // Exposure: the lock chip, the EV slider beside the ring (where
        // the settings keep it), the manual controls.
        let ex = &view.exposure;
        self.lock_chip.set_visible(ex.locked);
        self.low_light_chip.set_visible(view.low_light);
        self.place_banners();
        self.refresh_top_bar(&view);
        let bursting = view.capturing && matches!(self.capture_mode(), capture::Mode::Burst(_));
        if let Some(n) = self.merging.get() {
            self.burst_label.set_label(&format!("Merging {n} frames…"));
        } else if bursting {
            self.burst_label.set_label("Hold still…");
        }
        self.burst_chip
            .set_visible(bursting || self.merging.get().is_some());
        let ev_on = settings.tap_ev_slider
            && ex.caps.ev.is_some()
            && self
                .ev_shown_until
                .get()
                .is_some_and(|t| Instant::now() < t);
        let anchor = view
            .focus
            .and_then(|f| self.viewfinder.widget_point(f.u, f.v));
        if ev_on {
            if let Some((x, y)) = anchor {
                let w = self.viewfinder.width() as f32;
                let x = if x + 70.0 < w { x + 44.0 } else { x - 70.0 };
                self.ev_slider.set_margin_start(x.max(0.0) as i32);
                self.ev_slider.set_margin_top((y - 85.0).max(0.0) as i32);
            }
            self.setting_ev.set(true);
            self.ev_slider.set_value(ex.ev_user as f64);
            self.setting_ev.set(false);
        }
        self.ev_slider.set_visible(ev_on && anchor.is_some());
        self.manual.update(ex);
        let open = self.manual_revealer.reveals_child();
        let set = crate::manual::any_set(ex);
        let adjust = &self.lens_row.adjust;
        if adjust.is_active() != open {
            adjust.set_active(open);
        }
        if set {
            adjust.add_css_class("set");
        } else {
            adjust.remove_css_class("set");
        }
        // Night bursts take their own exposure: no manual values then.
        adjust.set_sensitive(!night);
        let upright = self.controls_turn.get() == 0;
        match crate::manual::summary(ex).filter(|_| !open && upright) {
            Some(text) => {
                self.manual_hint_label.set_label(&format!("● {text}"));
                self.manual_hint.set_visible(true);
            }
            None => self.manual_hint.set_visible(false),
        }
        // The shutter's ring while the flash is lit for the photo.
        if view.light == Light::Photo {
            self.shutter.add_css_class("flash");
        } else {
            self.shutter.remove_css_class("flash");
        }
        self.debug.set_visible(self.debug_enabled());
        self.viewfinder.set_measure(self.debug_enabled());
        if self.debug_enabled() {
            let fps = self.fps.borrow().value;
            // What reaches the screen: frames drawn, and the time from a
            // frame arriving to the compositor showing it.
            let screen = match self.viewfinder.paint_stats() {
                Some(p) => match p.latency_ms {
                    Some((mean, max)) => {
                        format!("\nscreen {:.1} fps  {mean:.0} ms (max {max:.0})", p.fps)
                    }
                    None => format!("\nscreen {:.1} fps", p.fps),
                },
                None => String::new(),
            };
            let (w, h) = self.shown_size.get();
            let switch = view
                .last_switch
                .map(|d| format!("{} ms", d.as_millis()))
                .unwrap_or_else(|| "-".into());
            self.debug.set_label(&format!(
                "{} {}x{}  {:.1} fps\nzoom {:.2}  switch {}{}{}",
                view.lens_name,
                w,
                h,
                fps,
                view.zoom,
                switch,
                if view.switching { "  (switching)" } else { "" },
                screen
            ));
        }
    }

    /// The quick settings: what each shows, and which belong to the mode.
    fn refresh_top_bar(&self, view: &omacam_core::controller::ViewState) {
        let settings = self.settings.borrow().clone();
        let video = view.mode == omacam_core::controller::Mode::Video;
        let night = self.ui_mode.get() == UiMode::Night;
        // Videos have no photo to flash for: the torch, or nothing.
        let flash = match view.flash_mode {
            FlashMode::Auto | FlashMode::On if video => FlashMode::Off,
            m => m,
        };
        self.flash_button.set_visible(view.can_flash);
        if !view.can_flash {
            self.flash_menu.set_reveal_child(false);
        }
        if let Some(i) = top_button_icon(&self.flash_button) {
            i.set_icon_name(Some(flash_icon(flash)));
        }
        if flash == FlashMode::Off {
            self.flash_button.remove_css_class("on");
        } else {
            self.flash_button.add_css_class("on");
        }
        for (mode, b) in &self.flash_choices {
            if *mode == flash {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
            // Videos have no photo to flash for.
            b.set_visible(!video || matches!(mode, FlashMode::Off | FlashMode::Torch));
        }
        self.timer_button.set_visible(!video);
        if let Some(l) = top_button_label(&self.timer_button) {
            let s = settings.timer.seconds();
            l.set_label(&format!("{s}s"));
            l.set_visible(s > 0);
        }
        if settings.timer == Timer::Off {
            self.timer_button.remove_css_class("on");
        } else {
            self.timer_button.add_css_class("on");
        }
        self.aspect_button.set_visible(!video);
        if video {
            self.aspect_menu.set_reveal_child(false);
        }
        for (a, b) in &self.aspect_choices {
            if *a == settings.aspect {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
        if let Some(l) = top_button_label(&self.aspect_button) {
            l.set_label(aspect_label(settings.aspect));
        }
        self.fps_toggle
            .set_visible(video && view.can_60fps && !view.recording);
        if self.fps_toggle.is_active() != (view.video_fps >= 60) {
            self.setting_fps.set(true);
            self.fps_toggle.set_active(view.video_fps >= 60);
            self.setting_fps.set(false);
        }
        self.hdr_toggle.set_visible(self.bursts && !video && !night);
        if self.hdr_toggle.is_active() != settings.hdr {
            self.setting_hdr.set(true);
            self.hdr_toggle.set_active(settings.hdr);
            self.setting_hdr.set(false);
        }
    }

    fn zoom_to(self: &Rc<Self>, zoom: f64, source: ZoomSource) {
        let now = Instant::now();
        if let Some(out) = self.with_controller(|c| c.set_zoom(zoom, source, now)) {
            self.apply(out);
        }
    }

    fn zoom_released(self: &Rc<Self>) {
        if let Some(out) = self.with_controller(|c| c.zoom_released(Instant::now())) {
            self.apply(out);
        }
    }

    /// Show a word over the middle of the picture for a moment (a lens's
    /// name).
    fn flash_note(self: &Rc<Self>, text: &str) {
        self.zoom_indicator.set_label(text);
        self.zoom_indicator.set_visible(true);
        if let Some(id) = self.hide_source.borrow_mut().take() {
            id.remove();
        }
        let a = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(HIDE_AFTER, move || {
            if let Some(a) = a.upgrade() {
                a.hide_source.borrow_mut().take();
                a.zoom_indicator.set_visible(false);
            }
        });
        *self.hide_source.borrow_mut() = Some(id);
    }

    /// Open a menu of the top bar under its button, closing the others.
    fn open_menu(&self, menu: &gtk::Revealer, button: &gtk::Button) {
        self.close_menus();
        if let Some(b) = button.compute_bounds(&self.viewfinder) {
            menu.set_margin_start((b.x() as i32 - 6).max(8));
        }
        menu.set_reveal_child(true);
    }

    /// Close the menus over the picture; whether one was open.
    fn close_menus(&self) -> bool {
        let menus = [&self.flash_menu, &self.aspect_menu, &self.lens_row.menu];
        let open = menus.iter().any(|m| m.reveals_child()) | self.lens_row.hide_dial();
        for m in menus {
            m.set_reveal_child(false);
        }
        open
    }

    /// The lens row: a lens from the menu, the dial.
    fn lens_event(self: &Rc<Self>, e: LensEvent) {
        let now = Instant::now();
        match e {
            LensEvent::Lens(lens) => {
                if let Some(out) = self.with_controller(|c| c.choose_lens(lens, now)) {
                    self.apply(out);
                }
            }
            LensEvent::Drag(z) => self.zoom_to(z, ZoomSource::Gesture),
            LensEvent::Released => self.zoom_released(),
        }
    }

    /// Choose a mode as the mode bar would.
    fn set_ui_mode(self: &Rc<Self>, mode: UiMode) {
        if let Some(i) = UI_MODES.iter().position(|m| *m == mode) {
            self.mode_bar.set_selected(i);
        }
        self.mode_chosen(mode);
    }

    /// The mode bar chose a mode.
    fn mode_chosen(self: &Rc<Self>, mode: UiMode) {
        let before = self.ui_mode.replace(mode);
        if mode == UiMode::Night && before != UiMode::Night {
            // A Night burst takes its own exposure: manual values and the
            // controls for them go.
            self.set_manual_open(false);
        }
        let cmode = if mode == UiMode::Video {
            omacam_core::controller::Mode::Video
        } else {
            omacam_core::controller::Mode::Photo
        };
        if let Some(out) = self.with_controller(|c| c.set_mode(cmode, Instant::now())) {
            self.apply(out);
        }
        self.push_capture_mode();
    }

    /// The flash mode, from the menu, a long press or a script; for this
    /// run only (every launch starts at auto).
    fn set_flash(self: &Rc<Self>, mode: FlashMode) {
        let now = self
            .controller
            .borrow()
            .as_ref()
            .map_or(self.settings.borrow().flash, |c| c.flash_mode());
        if mode == FlashMode::Torch && now != FlashMode::Torch {
            self.before_torch.set(now);
        }
        self.flash_menu.set_reveal_child(false);
        match self.with_controller(|c| c.set_flash_mode(mode)) {
            Some(out) => self.apply(out),
            None => self.refresh(),
        }
    }

    /// Change the settings and save them.
    fn change_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.settings.borrow().clone();
        f(&mut s);
        if let Err(e) = s.save(&self.settings_path) {
            self.error(&format!("Settings not saved: {e}"));
        }
        *self.settings.borrow_mut() = s;
    }

    /// Open or close the manual controls.
    fn set_manual_open(self: &Rc<Self>, open: bool) {
        self.manual_revealer.set_reveal_child(open);
        if !open {
            self.manual.select(None);
        }
        // While they're open the lens is the one chosen (no low-light
        // stand-in).
        if let Some(out) = self.with_controller(|c| c.set_pro(open, Instant::now())) {
            self.apply(out);
        }
        self.refresh();
    }

    fn connect_input(self: &Rc<Self>) {
        // Pinch anywhere on the viewfinder; it rests a moment at 1x.
        let pinch = gtk::GestureZoom::new();
        let a = Rc::downgrade(self);
        pinch.connect_begin(move |_, _| {
            if let Some(a) = a.upgrade() {
                let z = a
                    .controller
                    .borrow()
                    .as_ref()
                    .map(|c| c.view().zoom)
                    .unwrap_or(1.0);
                a.pinch_base.set(z);
            }
        });
        let a = Rc::downgrade(self);
        pinch.connect_scale_changed(move |_, s| {
            if let Some(a) = a.upgrade() {
                let z = crate::dial::snap(a.pinch_base.get() * s, &[1.0]);
                a.zoom_to(z, ZoomSource::Gesture);
            }
        });
        let a = Rc::downgrade(self);
        pinch.connect_end(move |_, _| {
            if let Some(a) = a.upgrade() {
                a.zoom_released();
            }
        });
        self.viewfinder.add_controller(pinch);
        // A mouse wheel zooms too (a laptop has no pinch on its screen).
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let a = Rc::downgrade(self);
        scroll.connect_scroll(move |_, _, dy| {
            let Some(a) = a.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let z = a
                .controller
                .borrow()
                .as_ref()
                .map(|c| c.view().zoom)
                .unwrap_or(1.0);
            let z = crate::dial::snap(z * 2f64.powf(-dy * 0.1), &[1.0]);
            a.zoom_to(z, ZoomSource::Gesture);
            a.zoom_released();
            glib::Propagation::Stop
        });
        self.viewfinder.add_controller(scroll);

        let a = Rc::downgrade(self);
        self.shutter.connect_clicked(move |_| {
            if let Some(a) = a.upgrade() {
                a.press_shutter();
            }
        });
        let a = Rc::downgrade(self);
        self.mode_bar.connect_selected(move |i| {
            if let Some(a) = a.upgrade() {
                a.mode_chosen(UI_MODES[i.min(UI_MODES.len() - 1)]);
            }
        });
        let a = Rc::downgrade(self);
        self.lens_row.adjust.connect_clicked(move |b| {
            if let Some(a) = a.upgrade() {
                a.set_manual_open(b.is_active());
            }
        });
        // The quick settings.
        let a = Rc::downgrade(self);
        self.flash_button.connect_clicked(move |_| {
            if let Some(a) = a.upgrade() {
                if a.flash_menu.reveals_child() {
                    a.flash_menu.set_reveal_child(false);
                } else {
                    a.open_menu(&a.flash_menu, &a.flash_button);
                }
            }
        });
        // A long press on the flash button switches the torch.
        let press = gtk::GestureLongPress::new();
        let a = Rc::downgrade(self);
        press.connect_pressed(move |g, _, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
            let Some(a) = a.upgrade() else { return };
            let now = a.with_controller(|c| c.flash_mode());
            if now == Some(FlashMode::Torch) {
                a.set_flash(a.before_torch.get());
            } else {
                a.set_flash(FlashMode::Torch);
            }
        });
        self.flash_button.add_controller(press);
        for (mode, b) in &self.flash_choices {
            let (a, mode) = (Rc::downgrade(self), *mode);
            b.connect_clicked(move |_| {
                if let Some(a) = a.upgrade() {
                    a.set_flash(mode);
                }
            });
        }
        let a = Rc::downgrade(self);
        self.timer_button.connect_clicked(move |_| {
            let Some(a) = a.upgrade() else { return };
            let next = match a.settings.borrow().timer {
                Timer::Off => Timer::Three,
                Timer::Three => Timer::Ten,
                Timer::Ten => Timer::Off,
            };
            a.change_settings(|s| s.timer = next);
            a.refresh();
        });
        let a = Rc::downgrade(self);
        self.aspect_button.connect_clicked(move |_| {
            let Some(a) = a.upgrade() else { return };
            if a.aspect_menu.reveals_child() {
                a.aspect_menu.set_reveal_child(false);
            } else {
                a.open_menu(&a.aspect_menu, &a.aspect_button);
            }
        });
        for (aspect, b) in &self.aspect_choices {
            let (a, aspect) = (Rc::downgrade(self), *aspect);
            b.connect_clicked(move |_| {
                let Some(a) = a.upgrade() else { return };
                a.aspect_menu.set_reveal_child(false);
                a.change_settings(|s| s.aspect = aspect);
                a.refresh();
            });
        }

        // Tap the picture to focus and meter there; a tap on the marker
        // of the point in force gives it up.
        let tap = gtk::GestureClick::new();
        let a = Rc::downgrade(self);
        tap.connect_released(move |_, n, x, y| {
            let Some(a) = a.upgrade() else { return };
            if n != 1 {
                return;
            }
            if a.close_menus() {
                return;
            }
            let marker = a
                .controller
                .borrow()
                .as_ref()
                .and_then(|c| c.view().tap_point)
                .and_then(|(u, v)| a.viewfinder.widget_point(u, v));
            if let Some((mx, my)) = marker {
                if (mx as f64 - x).hypot(my as f64 - y) < 40.0 {
                    a.clear_tap();
                    return;
                }
            }
            let Some((u, v)) = a.viewfinder.picture_point(x, y) else {
                return;
            };
            a.tap_at(u, v);
        });
        self.viewfinder.add_controller(tap);

        // Long press: AE/AF lock.
        let press = gtk::GestureLongPress::new();
        let a = Rc::downgrade(self);
        press.connect_pressed(move |_, x, y| {
            let Some(a) = a.upgrade() else { return };
            if a.viewfinder.picture_point(x, y).is_none() {
                return;
            }
            if let Some(out) = a.with_controller(|c| c.lock(Instant::now())) {
                a.apply(out);
            }
        });
        self.viewfinder.add_controller(press);

        // The EV slider beside the focus ring (where the settings keep it).
        let a = Rc::downgrade(self);
        self.ev_slider.connect_value_changed(move |s| {
            let Some(a) = a.upgrade() else { return };
            if a.setting_ev.get() {
                return;
            }
            a.ev_shown_until.set(Some(Instant::now() + EV_SHOWN));
            if let Some(out) = a.with_controller(|c| c.set_ev(s.value() as f32)) {
                a.apply(out);
            }
        });

        // Volume keys take a photo, when they reach the app at all: the
        // phone's shell binds them for the system volume, and hands them
        // over through `omacam --remote shutter` instead. With the fake
        // cameras, O turns the phone a quarter (as if held sideways).
        let keys = gtk::EventControllerKey::new();
        let a = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(a) = a.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let volume = matches!(key, gdk::Key::AudioLowerVolume | gdk::Key::AudioRaiseVolume);
            if volume && a.settings.borrow().volume_shutter {
                a.press_shutter();
                return glib::Propagation::Stop;
            }
            if a.cli.fake && matches!(key, gdk::Key::o | gdk::Key::O) {
                a.held.set(match a.held.get() {
                    DeviceOrientation::Normal => DeviceOrientation::LeftUp,
                    DeviceOrientation::LeftUp => DeviceOrientation::BottomUp,
                    DeviceOrientation::BottomUp => DeviceOrientation::RightUp,
                    _ => DeviceOrientation::Normal,
                });
                tracing::info!(held = ?a.held.get(), "held (O key)");
                a.turn_controls();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
        let a = Rc::downgrade(self);
        self.flip.connect_clicked(move |_| {
            if let Some(a) = a.upgrade() {
                if let Some(out) = a.with_controller(|c| c.flip(Instant::now())) {
                    a.apply(out);
                }
            }
        });
        let a = Rc::downgrade(self);
        self.thumb.connect_clicked(move |_| {
            let Some(a) = a.upgrade() else { return };
            let Some(path) = a.last_photo.borrow().clone() else {
                return;
            };
            let uri = gio::File::for_path(&path).uri();
            if let Err(e) =
                gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
            {
                a.error(&format!("Can't open the photo: {e}"));
            }
        });

        // Let other apps have the cameras while hidden.
        let a = Rc::downgrade(self);
        self.window.connect_suspended_notify(move |w| {
            if let Some(a) = a.upgrade() {
                tracing::info!(
                    suspended = w.is_suspended(),
                    active = w.is_active(),
                    "window state"
                );
                a.presence_changed();
            }
        });
        let a = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |w| {
            let Some(a) = a.upgrade() else { return };
            tracing::info!(
                suspended = w.is_suspended(),
                active = w.is_active(),
                "window state"
            );
            a.presence_changed();
            if w.is_active() {
                // Back in front (from the photo viewer, say): focus and
                // metering over the whole picture again, as shown.
                a.clear_tap();
            }
        });
    }

    /// The window came to the front, left it, or went out of sight or
    /// back. The flash LED goes out at once out of the front; the cameras
    /// go only after [`UNFOCUSED_RELEASE`] out of sight, and come back as
    /// soon as the window shows again.
    ///
    /// Out of sight is what the compositor says (phoc suspends a window
    /// covered for 3 s: another app on top, the lock screen); where it
    /// never does, out of focus stands in. A turn of the screen is
    /// neither, and keeps the stream.
    fn presence_changed(self: &Rc<Self>) {
        let w = &self.window;
        if w.is_suspended() {
            self.reports_suspended.set(true);
        }
        self.set_front(w.is_active() && !w.is_suspended());
        let hidden = if self.reports_suspended.get() {
            w.is_suspended()
        } else {
            !w.is_active()
        };
        if !hidden {
            if let Some(id) = self.unfocus_source.borrow_mut().take() {
                id.remove();
            }
            self.set_suspended(false);
        } else if self.unfocus_source.borrow().is_none() && !self.suspended.get() {
            let weak = Rc::downgrade(self);
            let id = glib::timeout_add_local_once(UNFOCUSED_RELEASE, move || {
                if let Some(a) = weak.upgrade() {
                    a.unfocus_source.borrow_mut().take();
                    tracing::info!("out of sight for a while: releasing the cameras");
                    a.set_suspended(true);
                }
            });
            *self.unfocus_source.borrow_mut() = Some(id);
        }
    }

    /// The window is in front or not (the flash LED is lit only in front).
    fn set_front(self: &Rc<Self>, front: bool) {
        self.front.set(front);
        if let Some(out) = self.with_controller(|c| c.set_foreground(front)) {
            if !out.is_empty() {
                self.apply(out);
            }
        }
    }

    /// Give up the tapped point: focus and metering over the whole picture.
    fn clear_tap(self: &Rc<Self>) {
        if let Some(out) = self.with_controller(|c| c.clear_tap()) {
            self.apply(out);
        }
    }

    fn set_suspended(self: &Rc<Self>, suspended: bool) {
        if suspended {
            self.stop_recording();
        }
        if self.suspended.replace(suspended) == suspended {
            return;
        }
        let now = Instant::now();
        let out = self.with_controller(|c| {
            if suspended {
                c.suspend(now)
            } else {
                c.resume(now)
            }
        });
        if let Some(out) = out {
            self.apply(out);
        }
    }

    fn tick(self: &Rc<Self>) {
        if self.screen.as_ref().is_some_and(|s| s.poll()) {
            self.turn_controls();
        }
        self.place_picture();
        if let Some(out) = self.with_controller(|c| c.tick(Instant::now())) {
            let focus = self.controller.borrow().as_ref().is_some_and(|c| {
                c.view().focus.is_some() || self.viewfinder.decor().ring.is_some()
            });
            if !out.is_empty() {
                self.apply(out);
            } else if self.debug_enabled() || focus || self.recording.borrow().is_some() {
                self.refresh();
            }
        }
    }

    /// The picture between the top bar and the bottom bar (the lens row,
    /// modes and shutter; the manual controls lie over the picture).
    fn place_picture(&self) {
        let vf = &self.viewfinder;
        let top = self
            .top_bar
            .compute_bounds(vf)
            .map(|b| b.y() + b.height())
            .unwrap_or(0.0);
        let bottom = self
            .bottom_fixed
            .compute_bounds(vf)
            .map(|b| vf.height() as f32 - b.y())
            .unwrap_or(0.0);
        vf.set_insets(Insets {
            top: top.max(0.0),
            bottom: bottom.max(0.0),
        });
    }

    /// Turn the controls for how the phone is held (the layout stays
    /// portrait; each control turns in place), and the picture half round
    /// where the layout lies upside down to the phone.
    ///
    /// Icons, lens buttons, chips and toasts turn; the words of the mode
    /// bar stay level, since turned they'd run into the rows above and
    /// below. The banners over the top of the picture (recording time,
    /// AE/AF lock, low light) move to the edge that is up, along it.
    fn turn_controls(&self) {
        let turns = self.turns_for(self.turned.turn());
        self.viewfinder.set_flipped(turns.picture == 180);
        let turn = turns.controls;
        // No words on their side: the modes' names only upright, the
        // line of manual values only then too.
        self.mode_bar.set_named(turn == 0);
        self.lens_row.set_turn(turn);
        self.manual_hint
            .set_visible(turn == 0 && self.manual_hint.is_visible());
        let old = self.controls_turn.replace(turn);
        if old != 0 {
            self.window.remove_css_class(&format!("turn-{old}"));
        }
        if turn != 0 {
            self.window.add_css_class(&format!("turn-{turn}"));
        }
        self.place_banners();
    }

    /// How the picture and the controls turn with the layout turned
    /// `layout` in the window ([`crate::turned::turns`]).
    fn turns_for(&self, layout: u32) -> crate::turned::Turns {
        let held = self.held.get().degrees();
        let screen = self.screen.as_ref().and_then(|s| {
            s.poll();
            s.turn()
        });
        let turns = crate::turned::turns(layout, screen, held);
        tracing::debug!(layout, ?screen, held, ?turns, "turns");
        turns
    }

    /// The banners along the edge that is up (their sizes change with
    /// their text, so this runs again as they change).
    fn place_banners(&self) {
        let turn = self.controls_turn.get();
        // Below the top bar when upright.
        let top = self.viewfinder.insets().top.max(52.0) as i32;
        let banners: [(&gtk::Widget, i32); 3] = [
            (self.rec_label.upcast_ref(), 14),
            (self.lock_chip.upcast_ref(), 50),
            (self.low_light_chip.upcast_ref(), 86),
        ];
        for (w, inset) in banners {
            let (_, width, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
            let (_, height, _, _) = w.measure(gtk::Orientation::Vertical, -1);
            // Turned a quarter, the banner's middle sits `inset` plus half
            // its height from the edge.
            let side = (inset + height / 2 - width / 2).max(0);
            w.set_margin_top(0);
            w.set_margin_start(0);
            w.set_margin_end(0);
            match turn {
                90 => {
                    w.set_halign(gtk::Align::Start);
                    w.set_valign(gtk::Align::Center);
                    w.set_margin_start(side);
                }
                270 => {
                    w.set_halign(gtk::Align::End);
                    w.set_valign(gtk::Align::Center);
                    w.set_margin_end(side);
                }
                _ => {
                    w.set_halign(gtk::Align::Center);
                    w.set_valign(gtk::Align::Start);
                    w.set_margin_top(top + inset - 6);
                }
            }
        }
    }

    /// Start or stop GeoClue with the "Location tags" setting.
    fn location_setting_changed(&self) {
        let on = self.settings.borrow().location;
        let running = self.locator.borrow().is_some();
        if on && !running {
            // GeoClue answers on its own thread; a failure comes back as a
            // message.
            let tx = self.tx.clone();
            *self.locator.borrow_mut() = Some(crate::location::Locator::start(move |why| {
                let _ = tx.send_blocking(Msg::NoLocation(why));
            }));
        } else if !on && running {
            self.locator.borrow_mut().take();
        }
    }

    /// The location for a photo or video now, when tags are on.
    fn location_fix(&self) -> Option<crate::location::Fix> {
        if !self.settings.borrow().location {
            return None;
        }
        self.locator.borrow().as_ref().and_then(|l| l.fix())
    }

    fn debug_enabled(&self) -> bool {
        self.cli.debug || self.settings.borrow().debug_overlay
    }

    /// The shutter button, a volume key or the control socket: start the
    /// self-timer if one is set, or cancel a running one.
    fn press_shutter(self: &Rc<Self>) {
        let video = self
            .controller
            .borrow()
            .as_ref()
            .is_some_and(|c| c.view().mode == omacam_core::controller::Mode::Video);
        if video {
            self.toggle_recording();
            return;
        }
        if let Some(id) = self.countdown.borrow_mut().take() {
            id.remove();
            self.zoom_indicator.set_visible(false);
            return;
        }
        let seconds = self.settings.borrow().timer.seconds();
        if seconds == 0 {
            self.take_photo();
            return;
        }
        self.countdown_left.set(seconds);
        self.zoom_indicator.set_label(&seconds.to_string());
        self.zoom_indicator.set_visible(true);
        let a = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(a) = a.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let left = a.countdown_left.get().saturating_sub(1);
            a.countdown_left.set(left);
            if left == 0 {
                a.countdown.borrow_mut().take();
                a.zoom_indicator.set_visible(false);
                a.take_photo();
                return glib::ControlFlow::Break;
            }
            a.zoom_indicator.set_label(&left.to_string());
            glib::ControlFlow::Continue
        });
        *self.countdown.borrow_mut() = Some(id);
    }

    fn toggle_recording(self: &Rc<Self>) {
        if self.recording.borrow().is_some() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    fn start_recording(self: &Rc<Self>) {
        let Some(camera) = self
            .controller
            .borrow()
            .as_ref()
            .and_then(|c| c.recording_source())
        else {
            return;
        };
        if self.cli.display != crate::Display::Memory {
            self.error("Video recording needs the memory display for now (--display memory)");
            return;
        }
        let Some((w, h, stride, layout)) = self.shown_format.get() else {
            return;
        };
        let Some(codec) = omacam_record::best_codec() else {
            self.error("No video encoder: install gst-plugins-ugly (x264)");
            return;
        };
        let settings = self.settings.borrow().clone();
        let dir = self
            .cli
            .output_dir
            .clone()
            .unwrap_or_else(crate::settings::default_video_dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error(&format!("Can't make {}: {e}", dir.display()));
            return;
        }
        let now = glib::DateTime::now_local().ok();
        let stamp = now
            .as_ref()
            .and_then(|d| d.format("%Y%m%d_%H%M%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "video".into());
        let created = now
            .as_ref()
            .and_then(|d| d.format_iso8601().ok())
            .map(|s| s.to_string());
        let Some(lenses) = self.rec_lenses(&camera) else {
            return;
        };
        let Some(lens) = lenses.get(&camera).copied() else {
            return;
        };
        let front = lens.mirror;
        let held = self.held.get();
        let stabilizer = self
            .stabilizing(&camera)
            .then(|| Arc::new(Mutex::new(omacam_core::stabilize::Stabilizer::default())));
        let gyro = stabilizer
            .as_ref()
            .and_then(|s| crate::sensors::gyro::Gyroscope::claim(s.clone()));
        let stabilizer = stabilizer.filter(|_| gyro.is_some());
        // 60 fps when asked for, offered, and the camera really delivers
        // it; else 30.
        let view = self.controller.borrow().as_ref().map(|c| c.view());
        let measured = self.fps.borrow().value;
        let want60 = settings.video_fps >= 60 && view.as_ref().is_some_and(|v| v.can_60fps);
        let fps = if want60 && measured >= 50.0 { 60 } else { 30 };
        if want60 && fps == 30 {
            self.error(&format!(
                "60 fps not reached ({measured:.0} fps): recording at 30 fps"
            ));
        }
        let location = settings
            .location
            .then(|| self.locator.borrow().as_ref().and_then(|l| l.fix()))
            .flatten()
            .map(|f| omacam_record::Location {
                latitude: f.latitude,
                longitude: f.longitude,
                altitude: f.altitude,
            });
        let (out_w, out_h) = settings.video_resolution.size();
        let config = omacam_record::Config {
            path: omacam_record::next_path(&dir, &stamp, codec.extension()),
            codec,
            input: omacam_record::Input {
                format: input_format(layout),
                width: w,
                height: h,
                stride,
                coverage: lens.coverage,
                flip: false,
            },
            out_width: out_w,
            out_height: out_h,
            fps,
            // Turned for how the phone is held when recording starts.
            rotation: orientation::turn(lens.rotation, held, front),
            created,
            location,
            audio: omacam_record::Audio::PipeWire,
        };
        match omacam_record::Recorder::start(config) {
            Ok(recorder) => {
                let tap = recorder.tap();
                tap.set_view(lens.view);
                *self.rec_tap.lock().unwrap() = Some(RecLink {
                    tap,
                    camera: camera.clone(),
                    lenses,
                    base_rotation: lens.rotation,
                    stabilizer,
                });
                // Focus once as recording starts (continuous AF holds once
                // it has found focus; a fresh scan catches a new subject).
                let now = Instant::now();
                if let Some(out) = self.with_controller(|c| c.refocus(now)) {
                    self.apply(out);
                }
                *self.recording.borrow_mut() = Some(Recording {
                    recorder,
                    since: Instant::now(),
                    _gyro: gyro,
                });
                self.with_controller(|c| c.set_recording(true));
                if self.settings.borrow().shutter_sound {
                    play_shutter_sound();
                }
            }
            Err(e) => self.error(&format!("Recording didn't start: {e}")),
        }
        self.refresh();
    }

    /// Stabilization for video from `camera`: asked for, a gyroscope, and
    /// a back camera (the selfie camera's frames are mirrored and it is
    /// usually held still at arm's length).
    fn stabilizing(&self, camera: &str) -> bool {
        self.settings.borrow().stabilization
            && self.has_gyro
            && self
                .controller
                .borrow()
                .as_ref()
                .and_then(|c| c.table().index_of(camera).map(|i| c.table().get(i).role))
                .is_some_and(|r| r != Role::Front)
    }

    /// How much tighter the video's view is than the picture's: the
    /// stabilization margin, in video mode, so the viewfinder shows what
    /// the file will.
    fn video_margin(&self, camera: &str) -> f64 {
        let video = self
            .controller
            .borrow()
            .as_ref()
            .is_some_and(|c| c.view().mode == omacam_core::controller::Mode::Video);
        if video && self.stabilizing(camera) {
            1.0 + omacam_core::stabilize::MARGIN
        } else {
            1.0
        }
    }

    /// The cameras a recording from `camera` may take frames from (the
    /// back ones, or the front one alone), with their views now.
    fn rec_lenses(&self, camera: &str) -> Option<std::collections::HashMap<String, RecLens>> {
        let c = self.controller.borrow();
        let c = c.as_ref()?;
        let table = c.table();
        let first = table.get(table.index_of(camera)?);
        let mut out = std::collections::HashMap::new();
        for lens in table.lenses() {
            if lens.role.faces_front() != first.role.faces_front() {
                continue;
            }
            let Some(t) = c.transform(&lens.id) else {
                continue;
            };
            let margin = self.video_margin(&lens.id);
            // The picture's upright shape, for the focal length's share.
            let shape = lens.still.or(lens.preview).map(|s| {
                let (w, h) = omacam_core::framing::upright_pair(
                    (s.width as f64, s.height as f64),
                    t.rotation,
                );
                w / h.max(1.0)
            });
            let focal = omacam_core::stabilize::focal_over_sides(
                lens.focal_length_35mm.unwrap_or(25) as f64,
                shape.unwrap_or(0.75),
            );
            out.insert(
                lens.id.clone(),
                RecLens {
                    rotation: t.rotation,
                    mirror: t.mirror,
                    coverage: lens.shown_coverage(),
                    view: rec_view(&t, margin),
                    focal,
                    scale: t.scale * margin,
                },
            );
        }
        Some(out)
    }

    /// Stop and finish the file off the UI thread. `wait` finishes it here
    /// (at shutdown).
    fn finish_recording(self: &Rc<Self>, wait: bool) {
        let Some(rec) = self.recording.borrow_mut().take() else {
            return;
        };
        let camera = self.rec_tap.lock().unwrap().take().map(|l| l.camera);
        self.with_controller(|c| c.set_recording(false));
        self.rec_label.set_visible(false);
        // The last frame becomes the gallery button's picture.
        if let Some(tex) = self.viewfinder.texture() {
            let mut d = gdk::TextureDownloader::new(&tex);
            d.set_format(gdk::MemoryFormat::B8g8r8a8);
            let (bytes, stride) = d.download_bytes();
            let (w, h) = (tex.width() as u32, tex.height() as u32);
            let mut data = Vec::with_capacity((w * h * 4) as usize);
            for row in bytes.chunks(stride).take(h as usize) {
                data.extend_from_slice(&row[..(w * 4) as usize]);
            }
            let img = photo::Bgrx {
                width: w,
                height: h,
                data,
            };
            let rotation = self.viewfinder.placement().rotation;
            let _ = &camera;
            let raw = photo::Raw {
                data: &img.data,
                width: w,
                height: h,
                stride: w * 4,
                layout: PixelLayout::Xrgb8888,
            };
            if let Ok(upright) = photo::crop_and_rotate(&raw, 1.0, rotation) {
                let t = photo::thumbnail(&upright);
                let tex = gdk::MemoryTexture::new(
                    t.width as i32,
                    t.height as i32,
                    gdk::MemoryFormat::B8g8r8x8,
                    &glib::Bytes::from_owned(t.data),
                    t.width as usize * 4,
                );
                self.thumb_picture.set_paintable(Some(&tex));
                self.thumb_picture.set_pixel_size(56);
            }
        }
        if wait {
            let _ = rec.recorder.stop();
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = rec.recorder.stop().map_err(|e| e.to_string());
            let _ = tx.send_blocking(Msg::VideoSaved(result));
        });
        self.refresh();
    }

    fn stop_recording(self: &Rc<Self>) {
        self.finish_recording(false);
    }

    fn take_photo(self: &Rc<Self>) {
        if let Some(out) = self.with_controller(|c| c.shutter(Instant::now())) {
            self.apply(out);
        }
    }

    /// A tap on the picture at (u, v): focus and meter there.
    fn tap_at(self: &Rc<Self>, u: f64, v: f64) {
        let luma = self.luma_around(u, v);
        let now = Instant::now();
        if let Some(out) = self.with_controller(|c| c.tap(u, v, luma, now)) {
            self.apply(out);
        }
        self.ev_shown_until.set(Some(now + EV_SHOWN));
        self.refresh();
    }

    /// The mean luma (0-255) of the frame around a point of the picture.
    fn luma_around(&self, u: f64, v: f64) -> Option<f64> {
        let camera = self.shown_camera.borrow().clone()?;
        let t = self.controller.borrow().as_ref()?.transform(&camera)?;
        let (x, y) = omacam_core::focus::picture_to_frame(u, v, &t);
        let pixels = self.last_pixels.borrow();
        let (px, w, h, stride, layout) = pixels.as_ref()?;
        let px = Pixels::new(px.as_ref(), *w, *h, *stride, *layout)?;
        Some(region_luma(&px, x, y, 0.04))
    }

    /// The manual controls: a value set by hand or given back to auto.
    fn manual_action(self: &Rc<Self>, action: crate::manual::Action) {
        use crate::manual::Action;
        let out = self.with_controller(|c| match action {
            Action::Ev(ev) => c.set_ev(ev),
            Action::Shutter(s) => c.set_shutter(s),
            Action::Iso(s) => c.set_iso(s),
            Action::WhiteBalance(s) => c.set_white_balance(s),
            Action::Focus(s) => c.set_lens_position(s),
            Action::AllAuto => c.reset_exposure(),
            Action::Histogram(_) => Vec::new(),
        });
        if let Action::Histogram(on) = action {
            self.histogram.set(on);
        }
        if let Some(out) = out {
            self.apply(out);
        }
    }

    fn open_settings(self: &Rc<Self>) {
        let a = Rc::downgrade(self);
        let dialog = crate::prefs::dialog(&self.settings.borrow(), move |s| {
            let Some(a) = a.upgrade() else { return };
            *a.settings.borrow_mut() = s.clone();
            if let Err(e) = s.save(&a.settings_path) {
                a.error(&format!("Settings not saved: {e}"));
            }
            a.with_controller(|c| c.set_tap_to_focus(s.tap_to_focus));
            a.turned.set_shutter_left(s.shutter_left);
            let now = Instant::now();
            let out = a.with_controller(|c| {
                let mut out = c.set_low_light(s.low_light, now);
                out.extend(c.set_hold_lens(!s.switch_lenses_by_zoom, now));
                out.extend(c.set_video_fps(s.video_fps));
                out
            });
            if let Some(out) = out {
                a.apply(out);
            }
            a.location_setting_changed();
            a.refresh();
        });
        // A phone-width window gets the sheet from the bottom, in reach of
        // the thumb (libadwaita's own switch happens below 450 px).
        if self.window.width() < 600 {
            dialog.set_presentation_mode(adw::DialogPresentationMode::BottomSheet);
            dialog.set_content_height(self.window.height() * 3 / 4);
        }
        // Nothing in the camera's window takes text: when the sheet goes,
        // no focus is left that could ask for the on-screen keyboard.
        let a = Rc::downgrade(self);
        dialog.connect_closed(move |_| {
            if let Some(a) = a.upgrade() {
                if let Ok(mut d) = a.settings_dialog.try_borrow_mut() {
                    d.take();
                }
                GtkWindowExt::set_focus(&a.window, None::<&gtk::Widget>);
            }
        });
        dialog.present(Some(&self.window));
        *self.settings_dialog.borrow_mut() = Some(dialog);
    }

    /// Wait until the window has painted a frame (at most 500 ms), so its
    /// render nodes are fresh for a snapshot.
    async fn shot_after_paint(self: &Rc<Self>, path: PathBuf) -> anyhow::Result<()> {
        let Some(clock) = self.window.frame_clock() else {
            return self.render_png(&path);
        };
        let (tx, rx) = async_channel::bounded::<anyhow::Result<()>>(2);
        let done = Rc::new(Cell::new(false));
        let (app, p, t, d) = (Rc::downgrade(self), path.clone(), tx.clone(), done.clone());
        // Snapshot inside the paint, while the render nodes are fresh.
        let id = clock.connect_after_paint(move |_| {
            if d.replace(true) {
                return;
            }
            if let Some(a) = app.upgrade() {
                let _ = t.try_send(a.render_png(&p));
            }
        });
        // No paint (the panel is off): render what there is.
        let app = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            if done.replace(true) {
                return;
            }
            if let Some(a) = app.upgrade() {
                let _ = tx.try_send(a.render_png(&path));
            }
        });
        let result = rx
            .recv()
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("no result")));
        clock.disconnect(id);
        result
    }

    /// Render the window's content to a PNG, the way it is drawn on screen.
    /// With the panel off the compositor sends no frames and GTK lays
    /// nothing out; then only the viewfinder is drawn, at the default
    /// window size.
    fn render_png(&self, path: &std::path::Path) -> anyhow::Result<()> {
        // The whole window, so a dialog over the content shows too.
        let content: gtk::Widget = self.window.clone().upcast();
        let (mut w, mut h) = (content.width() as f32, content.height() as f32);
        let mut node = None;
        if w > 0.0 && h > 0.0 && content.is_mapped() {
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(&content)).snapshot(&snapshot, w as f64, h as f64);
            node = snapshot.to_node();
        }
        if node.is_none() {
            tracing::info!(
                mapped = content.is_mapped(),
                "window not drawn (panel off?): drawing the viewfinder alone"
            );
            if w <= 0.0 || h <= 0.0 {
                let (dw, dh) = self.window.default_size();
                (w, h) = (dw as f32, dh as f32);
            }
            let snapshot = gtk::Snapshot::new();
            crate::viewfinder::draw(
                &snapshot,
                (w, h),
                1.0,
                self.viewfinder.insets(),
                self.viewfinder.texture().as_ref(),
                self.viewfinder.placement(),
                &self.viewfinder.decor(),
                None,
                self.viewfinder.flipped(),
            );
            node = snapshot.to_node();
        }
        let node = node.ok_or_else(|| anyhow::anyhow!("nothing drawn"))?;
        let renderer = match self.window.renderer() {
            Some(r) => r,
            None => {
                let r = gtk::gsk::CairoRenderer::new();
                r.realize_for_display(&WidgetExt::display(&self.window))?;
                r.upcast()
            }
        };
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, w, h);
        let texture = renderer.render_texture(node, Some(&bounds));
        texture.save_to_png(path)?;
        Ok(())
    }

    /// One log line with what the debug overlay shows.
    fn log_state(&self) {
        let Some(view) = self.controller.borrow().as_ref().map(|c| c.view()) else {
            return;
        };
        let (w, h) = self.shown_size.get();
        tracing::info!(
            lens = %view.lens_name,
            zoom = view.zoom,
            size = %format!("{w}x{h}"),
            fps = format!("{:.1}", self.fps.borrow().value),
            switch_ms = view.last_switch.map(|d| d.as_millis() as u64),
            switching = view.switching,
            capturing = view.capturing,
            luma = self.shown_luma.get().map(|l| format!("{l:.0}")),
            exposure_us = self.shown_ae.get().0,
            gain = self.shown_ae.get().1,
            digital = self.shown_ae.get().2,
            "state"
        );
    }

    fn shutdown(self: &Rc<Self>) {
        self.finish_recording(true);
        self.suspended.set(true);
        self.freeze_in_memory();
        self.frame_slot.lock().unwrap().take();
        if let Some(out) = self.with_controller(|c| c.suspend(Instant::now())) {
            for o in out {
                if let Output::Backend(cmd) = o {
                    if let Some(b) = self.backend.borrow().as_ref() {
                        b.send(cmd);
                    }
                }
            }
        }
        if let Some(led) = &self.flash_led {
            led.set(omacam_core::flash::Light::Off);
        }
        // Dropping the backend joins its thread after the stop.
        self.backend.borrow_mut().take();
    }
}

/// How to turn a captured frame into the saved photo.
struct PhotoOptions {
    /// Clockwise degrees from the frame to the upright photo, the way the
    /// phone was held included.
    turn: u32,
    quality: i32,
    /// Width over height, upright; None keeps the frame's shape.
    aspect: Option<f64>,
    /// The same shape, upright for a phone held in portrait.
    portrait_aspect: Option<f64>,
    /// Half the width and height.
    medium: bool,
    /// Mirror left-right (front camera photos saved as previewed).
    mirror: bool,
    /// Keep the frame's pixels as they are and record the turn and the
    /// mirror as the EXIF orientation.
    exif_orientation: bool,
    /// The hardware JPEG encoder's V4L2 name, when the pixels stay as they
    /// are; the CPU encodes when it is None or fails.
    jpeg_encoder: Option<String>,
    /// Device and time; the rest is filled from the picture.
    exif: exif::Info,
}

fn encode_and_write(
    p: &Picture,
    dir: &std::path::Path,
    stamp: &str,
    opts: PhotoOptions,
) -> anyhow::Result<(PathBuf, photo::Bgrx)> {
    let Some(data) = p.frame.data.bytes() else {
        anyhow::bail!("the still frame wasn't copied to memory");
    };
    let started = Instant::now();
    let raw = photo::Raw {
        data,
        width: p.frame.size.width,
        height: p.frame.size.height,
        stride: p.frame.stride,
        layout: p.frame.layout,
    };
    // The view's window of the frame (zoom, its center, the photo's
    // shape, upright as the phone is held in portrait), then turned as
    // the phone was held.
    let window =
        omacam_core::framing::frame_window(&p.transform, p.frame.size, opts.portrait_aspect);
    let rect = photo::window_rect(raw.width, raw.height, window);
    let mut encoder = "software";
    let (jpeg, width, height, thumb, orientation) = if opts.exif_orientation {
        // The pixels stay as the frame has them: crop in the frame's
        // terms, and tell viewers how to turn it.
        let rect = photo::rect_to_aspect(rect, opts.aspect, opts.turn);
        let hw = opts.jpeg_encoder.as_deref().and_then(|name| {
            crate::hwjpeg::encode(name, &raw, rect, opts.medium, opts.quality)
                .inspect_err(|e| tracing::warn!("{e:#}; encoding on the CPU"))
                .ok()
        });
        let (jpeg, width, height) = match hw {
            Some(e) => {
                encoder = "hardware";
                (e.jpeg, e.width, e.height)
            }
            None => {
                let img = photo::crop_window_and_rotate(&raw, rect, 0)?;
                let img = if opts.medium { photo::half(&img) } else { img };
                (
                    photo::encode_jpeg(&img, opts.quality)?,
                    img.width,
                    img.height,
                )
            }
        };
        let thumb = photo::thumbnail_of(&raw, rect, opts.turn, opts.mirror)?;
        let orientation = photo::exif_orientation(opts.turn, opts.mirror);
        (jpeg, width, height, thumb, orientation)
    } else {
        let img = photo::crop_window_and_rotate(&raw, rect, opts.turn)?;
        let mut img = photo::crop_to_aspect(img, opts.aspect);
        if opts.mirror {
            photo::mirror(&mut img);
        }
        let img = if opts.medium { photo::half(&img) } else { img };
        let jpeg = photo::encode_jpeg(&img, opts.quality)?;
        (jpeg, img.width, img.height, photo::thumbnail(&img), 1)
    };
    let m = &p.frame.meta;
    let gain = m.analog_gain.unwrap_or(1.0) as f64 * m.digital_gain.unwrap_or(1.0) as f64;
    let info = exif::Info {
        width,
        height,
        orientation,
        exposure_us: m.exposure_us,
        iso: m
            .analog_gain
            .map(|_| omacam_core::exposure_control::iso(gain, p.lens.base_iso)),
        manual_exposure: !p.settings.shutter.is_auto() || !p.settings.gain.is_auto(),
        manual_white_balance: !p.settings.white_balance.is_auto(),
        exposure_bias: Some(p.settings.ev),
        flash: Some(p.flash),
        f_number: p.lens.f_number,
        // The crop narrows the view like a longer lens.
        focal_length_35mm: p
            .lens
            .focal_length_35mm
            .map(|f| (f as f64 * p.crop).round() as u32),
        digital_zoom: Some(p.crop),
        focal_length_mm: p.lens.focal_length_mm,
        ..opts.exif
    };
    let jpeg = exif::insert(&jpeg, &info);
    std::fs::create_dir_all(dir)?;
    let path = photo::next_path(dir, stamp);
    photo::write_atomically(&path, &jpeg)?;
    tracing::info!(
        lens = %p.lens.name,
        zoom = p.zoom,
        size = %format!("{width}x{height}"),
        orientation,
        encoder,
        bytes = jpeg.len(),
        ms = started.elapsed().as_millis() as u64,
        color_gains = ?p.frame.meta.color_gains,
        exposure_us = ?p.frame.meta.exposure_us,
        gain = ?p.frame.meta.analog_gain,
        "photo encoded"
    );
    Ok((path, thumb))
}

/// The freedesktop camera shutter sound, through libcanberra's player, or
/// PipeWire's or PulseAudio's player on the sound file. Fire and forget.
fn play_shutter_sound() {
    const FILE: &str = "/usr/share/sounds/freedesktop/stereo/camera-shutter.oga";
    let argv: Option<Vec<&str>> = if glib::find_program_in_path("canberra-gtk-play").is_some() {
        Some(vec![
            "canberra-gtk-play",
            "-i",
            "camera-shutter",
            "-d",
            "Camera",
        ])
    } else if std::path::Path::new(FILE).exists() {
        ["pw-play", "paplay"]
            .into_iter()
            .find(|p| glib::find_program_in_path(p).is_some())
            .map(|p| vec![p, FILE])
    } else {
        None
    };
    let Some(argv) = argv else {
        tracing::debug!("no way to play the shutter sound");
        return;
    };
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    if let Err(e) = gio::Subprocess::newv(
        &argv,
        gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE,
    ) {
        tracing::debug!(error = %e, "shutter sound");
    }
}

/// `$XDG_RUNTIME_DIR/omacam/control`: one command per line, from
/// `omacam --remote CMD` (the phone shell's volume keys send `shutter`).
pub fn control_socket_path() -> PathBuf {
    glib::user_runtime_dir().join("omacam/control")
}

fn start_control_socket(tx: async_channel::Sender<Msg>) {
    use std::io::BufRead;
    let path = control_socket_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::remove_file(&path);
    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "no control socket");
            return;
        }
    };
    let _ = std::thread::Builder::new()
        .name("omacam-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                for line in std::io::BufReader::new(stream)
                    .lines()
                    .map_while(Result::ok)
                {
                    let cmd = line.trim().to_string();
                    if !cmd.is_empty() && tx.send_blocking(Msg::Remote(cmd)).is_err() {
                        return;
                    }
                }
            }
        });
}

/// The mean luma (0-255) of the square around frame point (x, y), 0..1,
/// `size` of the shorter side across, in 4-byte BGRX/RGBX pixels (the
/// green channel and the two others weigh the same in both orders).
fn region_luma(px: &Pixels<'_>, x: f64, y: f64, size: f64) -> f64 {
    let (w, h) = (px.width, px.height);
    let half = ((w.min(h) as f64 * size) / 2.0).max(1.0);
    let (cx, cy) = (x * w as f64, y * h as f64);
    let x0 = (cx - half).max(0.0) as u32;
    let x1 = ((cx + half) as u32).min(w.saturating_sub(1));
    let y0 = (cy - half).max(0.0) as u32;
    let y1 = ((cy + half) as u32).min(h.saturating_sub(1));
    let (mut sum, mut n) = (0.0, 0u32);
    let step = ((x1 - x0) / 32).max(1);
    for yy in (y0..=y1).step_by(step as usize) {
        for xx in (x0..=x1).step_by(step as usize) {
            sum += px.luma(xx, yy);
            n += 1;
        }
    }
    if n == 0 {
        MID_LUMA
    } else {
        sum / n as f64
    }
}

const MID_LUMA: f64 = 118.0;

/// A luma histogram of a 4-byte-per-pixel frame, `bins` bins scaled so the
/// fullest is 1, from a sparse sample.
fn luma_histogram(px: &Pixels<'_>, bins: usize) -> Vec<f32> {
    let mut counts = vec![0u32; bins];
    for y in (0..px.height).step_by(8) {
        for x in (0..px.width).step_by(8) {
            let l = px.luma(x, y) as f32;
            counts[((l / 256.0) * bins as f32) as usize % bins] += 1;
        }
    }
    let max = counts.iter().copied().max().unwrap_or(1).max(1) as f32;
    counts.into_iter().map(|c| c as f32 / max).collect()
}
