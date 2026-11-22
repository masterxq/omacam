//! The controller: user input and backend events in, backend commands and
//! things to show out. The UI owns one and does what it returns.

use std::time::{Duration, Instant};

use crate::backend::{Awb, Caps, ExposureSettings, Setting};
use crate::backend::{CameraInfo, Command, Event, Frame, FrameMeta, Purpose, StreamRequest};
use crate::capture::{self, CaptureController, Step};
use crate::exposure;
use crate::exposure_control::{self, Exposure};
use crate::flash::{FlashMode, Light, PreFlash, PreFlashStep};
use crate::focus::{self, FocusController};
use crate::framing;
pub use crate::framing::Transform;
use crate::lens::{Facing, Lens, LensTable, Role, TableSpec};
use crate::lowlight::{self, LowLight};
use crate::switch::{Action, FrameKind, SwitchController, Target};
use crate::zoom::{Ladder, ZoomPlanner, ZoomSource};

/// How long to wait before trying a camera again after it failed to start
/// (another app may hold it).
pub const RETRY_AFTER: Duration = Duration::from_secs(2);

/// Preset zoom buttons, as CAMERA-APP-PLAN §3.4 has them. A preset just
/// above the widest lens (0.5 over 0.45) snaps to that lens's full view.
pub const BACK_PRESETS: [f64; 4] = [0.5, 1.0, 2.0, 5.0];
pub const FRONT_PRESETS: [f64; 2] = [1.0, 2.0];
pub const MACRO_PRESETS: [f64; 3] = [1.0, 2.0, 4.0];

#[derive(Debug, Clone)]
pub struct Options {
    pub capture_mode: capture::Mode,
    pub preview_buffers: u32,
    pub still_buffers: u32,
    /// Start a new lens with the old one's exposure, scaled by aperture.
    pub transfer_exposure: bool,
    /// Taps on the picture focus there.
    pub tap_to_focus: bool,
    /// In dim light, show the telephoto's zoom as a crop of the main
    /// camera ([`crate::lowlight`]).
    pub low_light: bool,
    /// Frames per second asked of a camera in video mode, where its mode
    /// allows it (30 or 60).
    pub video_fps: u32,
    /// Merge Night and HDR bursts from the raw frames, where the camera
    /// gives them; otherwise from the processed ones.
    pub raw_merge: bool,
    /// Hold the lens the user chose: zooming crops it and never switches
    /// to another camera, only the lens buttons do (no low-light stand-in
    /// either). Off, the zoom picks the lens ([`crate::zoom`]).
    pub hold_lens: bool,
    /// The back has a flash LED the app can light ([`crate::flash`]).
    pub flash: bool,
    /// What the flash does.
    pub flash_mode: FlashMode,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            capture_mode: capture::Mode::Full,
            preview_buffers: 4,
            still_buffers: 2,
            transfer_exposure: true,
            tap_to_focus: true,
            low_light: true,
            video_fps: 30,
            raw_merge: true,
            hold_lens: false,
            flash: false,
            flash_mode: FlashMode::Off,
        }
    }
}

/// A captured frame to encode and save.
#[derive(Debug)]
pub struct Picture {
    pub frame: Frame,
    /// How the frame shows the view: the zoom crop, its center, the
    /// mode's coverage, the turn upright.
    pub transform: Transform,
    /// Center crop: keep 1/crop of each side (1 = the whole frame).
    pub crop: f64,
    /// Clockwise degrees that turn the frame upright.
    pub rotation: u32,
    pub lens: Lens,
    pub zoom: f64,
    /// The exposure settings in force (EXIF records them).
    pub settings: ExposureSettings,
    /// The EXIF Flash tag ([`crate::flash::exif_flash`]).
    pub flash: u16,
}

/// What the UI should do.
#[derive(Debug)]
pub enum Output {
    Backend(Command),
    /// A new frame for the viewfinder.
    Show(Frame),
    Picture(Box<Picture>),
    /// One frame of a burst (Night, HDR) for the merge; more follow until
    /// [`Output::BurstDone`].
    BurstFrame(Box<Picture>),
    /// The burst's last frame was the one before: merge them.
    BurstDone,
    /// Light the flash LED so, or put it out ([`Light::Off`]). Sent when
    /// it changes.
    Light(Light),
    /// Something to tell the user.
    Error(String),
}

/// Photos or videos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Photo,
    Video,
}

/// The exposure state the Pro panel and the EV slider show.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExposureView {
    pub caps: Caps,
    /// What's sent to the camera now.
    pub settings: ExposureSettings,
    /// The Pro panel's own values (manual ones stay set across lenses).
    pub pro: ExposureSettings,
    /// The EV slider's offset.
    pub ev_user: f32,
    pub locked: bool,
    /// ISO at gain 1 for this lens.
    pub base_iso: f64,
    /// What the camera reported for the last frame (auto's choice).
    pub shutter_us: Option<i32>,
    pub iso: Option<u32>,
    pub lens_position: Option<f32>,
}

/// A button of the lens row: a lens, or a zoom stage of one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomButton {
    /// The zoom a tap goes to.
    pub zoom: f64,
    /// The lens that shows it (an index into the lens table).
    pub lens: usize,
    pub role: Role,
    /// The lens's own button (its widest); false for a stage that crops it.
    pub native: bool,
    /// The button of the chosen lens with the highest zoom at or below the
    /// current zoom (none while the macro lens is on).
    pub active: bool,
}

/// The state the controls show.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewState {
    pub facing: Facing,
    pub zoom: f64,
    pub min_zoom: f64,
    pub max_zoom: f64,
    /// The lowest zoom a gesture reaches now: the chosen lens's own view
    /// while the lens is held ([`Options::hold_lens`]), else `min_zoom`.
    pub zoom_floor: f64,
    pub presets: Vec<f64>,
    /// The lens row of the side the app looks to (the back's ladder while
    /// the macro lens is on, too).
    pub buttons: Vec<ZoomButton>,
    /// The lens that is wanted for the current zoom.
    pub lens_role: Option<Role>,
    pub lens_name: String,
    pub switching: bool,
    pub capturing: bool,
    pub can_flip: bool,
    /// The back side has a macro lens to turn on.
    pub can_macro: bool,
    /// The macro lens streams instead of the back side's zoom ladder.
    pub macro_on: bool,
    pub last_switch: Option<Duration>,
    /// Exposure, white balance and focus: what the camera offers, what's
    /// set, and what auto chose.
    pub exposure: ExposureView,
    /// The focus ring, after a tap.
    pub focus: Option<focus::Indicator>,
    /// The tapped point (0..1 on the picture) that focus and metering
    /// follow, as long as they do; None while they cover the whole
    /// picture.
    pub tap_point: Option<(f64, f64)>,
    pub mode: Mode,
    pub recording: bool,
    /// The telephoto's zoom is shown by the main camera because the light
    /// is too low for the tele.
    pub low_light: bool,
    /// The streaming camera can run at 60 fps in its current mode.
    pub can_60fps: bool,
    /// The frame rate video mode asks for now (30 or 60).
    pub video_fps: u32,
    /// What the flash does (the setting, kept on the front camera too).
    pub flash_mode: FlashMode,
    /// The flash can be used here: the phone has one and a back lens
    /// streams. (The front camera has none: its mode is kept but unused.)
    pub can_flash: bool,
    /// What the LED does now.
    pub light: Light,
}

/// The presets within a ladder's range; the ".5" button is the widest
/// lens's whole view (0.45x on the Pixel 7 Pro), as stock's is, not a crop
/// of it.
fn preset_values(presets: &[f64], min_zoom: f64, max_zoom: f64) -> Vec<f64> {
    presets
        .iter()
        .copied()
        .filter(|&p| p >= min_zoom - 0.06 && p <= max_zoom)
        .map(|p| {
            if p < 1.0 && p - min_zoom < 0.1 {
                min_zoom
            } else {
                p
            }
        })
        .collect()
}

struct Side {
    planner: ZoomPlanner,
    /// Ladder step to lens-table index.
    lenses: Vec<usize>,
    roles: Vec<Role>,
    /// (zoom factor, axis) of each step, for the view's center.
    axes: Vec<(f64, (f64, f64))>,
    /// Low light: the tele's step is shown by the main camera.
    fallback: bool,
}

impl Side {
    fn new(table: &LensTable, facing: Facing) -> Option<Side> {
        Side::of(table, table.ladder(facing), table.max_zoom(facing))
    }

    /// The macro lens alone, zoomed digitally.
    fn macro_lens(table: &LensTable) -> Option<Side> {
        let lens = table.by_role(Role::Macro)?;
        Side::of(table, vec![lens], crate::lens::MACRO_MAX_ZOOM)
    }

    fn of(table: &LensTable, lenses: Vec<usize>, max_zoom: f64) -> Option<Side> {
        if lenses.is_empty() {
            return None;
        }
        let factors = lenses.iter().map(|&i| table.get(i).zoom).collect();
        let ladder = Ladder::new(factors, max_zoom);
        let roles = lenses.iter().map(|&i| table.get(i).role).collect();
        let axes = lenses
            .iter()
            .map(|&i| (table.get(i).zoom, table.get(i).axis))
            .collect();
        // Open at 1x, or the widest lens when nothing reaches 1x.
        Some(Side {
            planner: ZoomPlanner::new(ladder, 1.0),
            lenses,
            roles,
            axes,
            fallback: false,
        })
    }

    /// The ladder's choice for the zoom, before any low-light fallback.
    fn planned(&self) -> usize {
        self.lenses[self.planner.target()]
    }

    /// The main camera stands in for the tele: low light, the tele's step.
    fn falls_back(&self) -> bool {
        self.fallback
            && self.roles[self.planner.target()] == Role::Tele
            && self.roles.contains(&Role::Main)
    }

    /// The lens that streams for the zoom.
    fn lens(&self) -> usize {
        if self.falls_back() {
            if let Some(main) = self.roles.iter().position(|&r| r == Role::Main) {
                return self.lenses[main];
            }
        }
        self.planned()
    }
}

/// The exposure after the torch went out (`Controller::light_out`).
#[derive(Debug, Clone)]
enum LightOut {
    /// The last frame under the light; no frame since.
    Pending(Option<FrameMeta>),
    Waiting(PreFlash),
}

pub struct Controller {
    table: LensTable,
    opts: Options,
    back: Option<Side>,
    front: Option<Side>,
    /// The back side's macro lens, outside its zoom ladder.
    macro_side: Option<Side>,
    /// The macro lens streams for the back side.
    macro_on: bool,
    facing: Facing,
    switch: SwitchController,
    capture: CaptureController,
    focus: FocusController,
    exposure: Exposure,
    last_meta: Option<(usize, FrameMeta)>,
    suspended: bool,
    retry_at: Option<Instant>,
    mode: Mode,
    recording: bool,
    low_light: LowLight,
    /// The Pro panel is open: the lens is the one the zoom picks, never a
    /// low-light stand-in.
    pro: bool,
    /// The highest frame rate each camera's current mode allows and its
    /// lowest where reported (from the stream's start), by camera id.
    frame_rates: std::collections::HashMap<String, (f64, Option<f64>)>,
    /// The running stream's generation when it has a still stream beside
    /// its preview, and that stream's size.
    beside: Option<(u64, crate::backend::Size)>,
    /// Cameras that started without the still stream asked beside their
    /// preview: they aren't asked again.
    no_beside: std::collections::HashSet<String>,
    /// The tapped point focus and metering follow ([`ViewState::tap_point`]).
    tap_point: Option<(f64, f64)>,
    /// The wait under the light before a flash photo.
    pre_flash: Option<PreFlash>,
    /// The LED is lit for the photo being taken.
    flash_photo: bool,
    /// What the LED was last told.
    light: Light,
    /// The torch went out while a back camera streams: the exposure walks
    /// back to the dark. The last frame under the light until the next
    /// frame starts the wait ([`PreFlash::light_out`]), then the wait.
    light_out: Option<LightOut>,
    /// A photo waits for the exposure to settle after the torch.
    light_out_photo: bool,
}

impl Controller {
    pub fn new(spec: &TableSpec, cameras: &[CameraInfo], opts: Options) -> Controller {
        let table = spec.resolve(cameras);
        let mut back = Side::new(&table, Facing::Back);
        let mut front = Side::new(&table, Facing::Front);
        let mut macro_side = Side::macro_lens(&table);
        for s in [&mut back, &mut front, &mut macro_side]
            .into_iter()
            .flatten()
        {
            s.planner.set_hold_lens(opts.hold_lens);
        }
        // The back camera, unless there's only a front one.
        let facing = if back.is_some() || front.is_none() {
            Facing::Back
        } else {
            Facing::Front
        };
        Controller {
            table,
            opts,
            back,
            front,
            macro_side,
            macro_on: false,
            facing,
            switch: SwitchController::new(),
            capture: CaptureController::new(),
            focus: FocusController::new(),
            exposure: Exposure::new(),
            last_meta: None,
            suspended: true,
            retry_at: None,
            mode: Mode::Photo,
            recording: false,
            low_light: LowLight::default(),
            pro: false,
            frame_rates: std::collections::HashMap::new(),
            beside: None,
            no_beside: std::collections::HashSet::new(),
            tap_point: None,
            pre_flash: None,
            flash_photo: false,
            light: Light::Off,
            light_out: None,
            light_out_photo: false,
        }
    }

    pub fn table(&self) -> &LensTable {
        &self.table
    }

    pub fn has_cameras(&self) -> bool {
        self.back.is_some() || self.front.is_some()
    }

    /// True between [`Controller::resume`] and [`Controller::suspend`].
    pub fn is_running(&self) -> bool {
        !self.suspended
    }

    /// The cameras changed (drivers loaded late, or one went away): stop
    /// what streams, take the new list, and, if running, start the
    /// default camera of the side in use (or of the other side when this
    /// one has none left). Stream generations carry on, so frames of the
    /// old stream can't pass for the new one's.
    pub fn set_cameras(
        &mut self,
        spec: &TableSpec,
        cameras: &[CameraInfo],
        now: Instant,
    ) -> Vec<Output> {
        let mut out = Vec::new();
        self.capture.abort();
        self.abort_flash();
        self.focus.reset();
        self.retry_at = None;
        self.last_meta = None;
        let table = spec.resolve(cameras);
        self.back = Side::new(&table, Facing::Back);
        self.front = Side::new(&table, Facing::Front);
        self.macro_side = Side::macro_lens(&table);
        let hold = self.opts.hold_lens;
        for s in [&mut self.back, &mut self.front, &mut self.macro_side]
            .into_iter()
            .flatten()
        {
            s.planner.set_hold_lens(hold);
        }
        self.macro_on &= self.macro_side.is_some();
        self.table = table;
        let table = &self.table;
        self.switch.remap(|id| table.index_of(id));
        if self.side().is_none() {
            self.facing = match self.facing {
                Facing::Back if self.front.is_some() => Facing::Front,
                Facing::Front if self.back.is_some() => Facing::Back,
                f => f,
            };
        }
        // A camera still present keeps streaming (targets are the same
        // stream by camera id); a gone one is replaced or stopped.
        if self.suspended {
            self.request(None, now, &mut out);
        } else {
            self.request_preview(now, &mut out);
            if self.side().is_none() {
                self.request(None, now, &mut out);
            }
        }
        self.sync_light(&mut out);
        out
    }

    fn side(&self) -> Option<&Side> {
        match self.facing {
            Facing::Back if self.macro_on => self.macro_side.as_ref(),
            Facing::Back => self.back.as_ref(),
            Facing::Front => self.front.as_ref(),
        }
    }

    fn side_mut(&mut self) -> Option<&mut Side> {
        match self.facing {
            Facing::Back if self.macro_on => self.macro_side.as_mut(),
            Facing::Back => self.back.as_mut(),
            Facing::Front => self.front.as_mut(),
        }
    }

    pub fn view(&self) -> ViewState {
        let side = self.side();
        let lens = side.map(|s| self.table.get(s.lens()));
        let (zoom, min_zoom, max_zoom) = side
            .map(|s| {
                let l = s.planner.ladder();
                (s.planner.zoom(), l.min(), l.max())
            })
            .unwrap_or((1.0, 1.0, 1.0));
        let presets = match self.facing {
            Facing::Back if self.macro_on => &MACRO_PRESETS[..],
            Facing::Back => &BACK_PRESETS[..],
            Facing::Front => &FRONT_PRESETS[..],
        };
        let zoom_floor = side.map(|s| s.planner.floor()).unwrap_or(min_zoom);
        ViewState {
            facing: self.facing,
            zoom,
            min_zoom,
            max_zoom,
            zoom_floor,
            presets: preset_values(presets, min_zoom, max_zoom),
            buttons: self.zoom_buttons(),
            lens_role: lens.map(|l| l.role),
            lens_name: lens.map(|l| l.name.clone()).unwrap_or_default(),
            switching: self.switch.is_switching(),
            capturing: self.busy(),
            can_flip: self.back.is_some() && self.front.is_some(),
            can_macro: self.facing == Facing::Back && self.macro_side.is_some(),
            macro_on: self.facing == Facing::Back && self.macro_on,
            last_switch: self.switch.last_switch_time(),
            exposure: self.exposure_view(),
            focus: self.focus.indicator(),
            tap_point: self.tap_point,
            mode: self.mode,
            recording: self.recording,
            low_light: side.is_some_and(|s| s.falls_back()),
            can_60fps: self.can_60fps(),
            video_fps: self.opts.video_fps,
            flash_mode: self.opts.flash_mode,
            can_flash: self.opts.flash && self.facing == Facing::Back,
            light: self.light,
        }
    }

    /// The lens row: the presets of the side's ladder, each shown by the
    /// lens the ladder gives it, and every lens no preset lands on at its
    /// own factor, so each lens has a button.
    fn zoom_buttons(&self) -> Vec<ZoomButton> {
        let (side, presets) = match self.facing {
            Facing::Back => (self.back.as_ref(), &BACK_PRESETS[..]),
            Facing::Front => (self.front.as_ref(), &FRONT_PRESETS[..]),
        };
        let Some(side) = side else {
            return Vec::new();
        };
        let ladder = side.planner.ladder();
        let mut zooms = preset_values(presets, ladder.min(), ladder.max());
        for step in 0..ladder.len() {
            if !zooms.iter().any(|&z| ladder.natural(z) == step) {
                zooms.push(ladder.factor(step));
            }
        }
        zooms.sort_by(f64::total_cmp);
        let mut buttons: Vec<ZoomButton> = Vec::new();
        for z in zooms {
            let step = ladder.natural(z);
            let lens = side.lenses[step];
            buttons.push(ZoomButton {
                zoom: z,
                lens,
                role: side.roles[step],
                native: !buttons.iter().any(|b| b.lens == lens),
                active: false,
            });
        }
        let chosen = side.planned();
        let shown = self.facing == Facing::Front || !self.macro_on;
        if shown {
            let zoom = side.planner.zoom();
            let active = buttons
                .iter()
                .rposition(|b| b.lens == chosen && b.zoom <= zoom + 1e-6)
                .or_else(|| buttons.iter().position(|b| b.lens == chosen));
            if let Some(i) = active {
                buttons[i].active = true;
            }
        }
        buttons
    }

    /// The streaming camera's mode allows 60 fps.
    fn can_60fps(&self) -> bool {
        self.switch
            .streaming()
            .and_then(|(_, t)| self.frame_rates.get(&t.camera))
            .is_some_and(|&(f, _)| f >= 59.0)
    }

    /// The frame durations to ask of a stream of `camera`, shortest and
    /// longest, microseconds. Video holds its frame rate: 60 fps when
    /// asked for and the mode allows it, else 30. Photos run at up to
    /// 30 fps and let the exposure lengthen frames as far as the mode
    /// allows, for dim scenes. Nothing where the camera doesn't report
    /// its frame durations (its own default, 30 fps).
    fn frame_duration_for(&self, camera: &str, purpose: Purpose) -> Option<(u32, u32)> {
        // Only on a camera whose mode reported its frame durations (it
        // takes FrameDurationLimits); elsewhere the camera keeps its own.
        let &(max, min) = self.frame_rates.get(camera)?;
        if self.mode == Mode::Video {
            let fast = purpose == Purpose::Preview && self.opts.video_fps >= 60 && max >= 59.0;
            let d = if fast { 16_667 } else { 33_333 };
            return Some((d, d));
        }
        // The longest frame the mode allows; 30 fps when it says none.
        let longest = min
            .filter(|&f| f > 0.0)
            .map(|f| (1e6 / f).round() as u32)
            .unwrap_or(33_333)
            .max(33_333);
        Some((33_333, longest))
    }

    /// Slider position (0..1) for the current zoom.
    pub fn slider_position(&self) -> f64 {
        self.side()
            .map(|s| s.planner.ladder().to_slider(s.planner.zoom()))
            .unwrap_or(0.0)
    }

    /// Slider position (0..1) of a zoom value.
    pub fn slider_for(&self, zoom: f64) -> f64 {
        self.side()
            .map(|s| s.planner.ladder().to_slider(zoom))
            .unwrap_or(0.0)
    }

    pub fn zoom_for_slider(&self, pos: f64) -> f64 {
        self.side()
            .map(|s| s.planner.ladder().from_slider(pos))
            .unwrap_or(1.0)
    }

    /// How to draw a preview frame from `camera` at the current zoom.
    pub fn transform(&self, camera: &str) -> Option<Transform> {
        self.transform_for(camera, Purpose::Preview)
    }

    /// How a frame of `camera`'s preview or still mode shows the current
    /// view. Every lens of the side frames the view alike (the scale is
    /// zoom over the lens's factor, about the center of [`framing`]); a
    /// frame from the other side (frozen across a flip) keeps its whole
    /// view. A frame is never drawn below 1x: a lens can't show more than
    /// its own field of view, so a frozen frame from a narrower lens holds
    /// at its full view until the wider lens's first frame.
    pub fn transform_for(&self, camera: &str, purpose: Purpose) -> Option<Transform> {
        let i = self.table.index_of(camera)?;
        let lens = self.table.get(i);
        let rotation = lens.upright_clockwise();
        let coverage = match purpose {
            Purpose::Preview => framing::upright_pair(lens.shown_coverage(), rotation),
            Purpose::Still => (1.0, 1.0),
        };
        let mut t = Transform {
            rotation,
            mirror: lens.mirror,
            coverage,
            ..Transform::default()
        };
        let Some(side) = self.side() else {
            return Some(t);
        };
        // A lens of another side (or the macro lens beside the ladder)
        // shows its whole view.
        if !side.lenses.contains(&i) {
            return Some(t);
        }
        let zoom = side.planner.zoom();
        t.scale = (zoom / lens.zoom).max(1.0);
        let center = framing::view_center(zoom, &side.axes);
        t.center = framing::lens_center(center, lens.zoom, lens.axis, t.scale);
        Some(t)
    }

    /// Time until [`Controller::tick`] has something to do.
    pub fn next_tick(&self, now: Instant) -> Option<Duration> {
        let zoom = self.side().and_then(|s| s.planner.next_tick(now));
        let retry = self.retry_at.map(|t| t.saturating_duration_since(now));
        let capture = self.busy().then_some(Duration::from_millis(100));
        let focus = self
            .focus
            .indicator()
            .is_some()
            .then_some(Duration::from_millis(100));
        [zoom, retry, capture, focus].into_iter().flatten().min()
    }

    fn target(&self, purpose: Purpose) -> Option<Target> {
        let side = self.side()?;
        let lens_idx = side.lens();
        let lens = self.table.get(lens_idx);
        let mut target = Target::new(lens_idx, purpose);
        target.camera = lens.id.clone();
        if purpose == Purpose::Preview {
            target.video = self.mode == Mode::Video && lens.video.is_some();
            // A photo from the running preview, where the camera can run
            // the still mode's stream beside it: no stop and start.
            let larger = match (lens.still, lens.preview) {
                (Some(s), Some(p)) => s.pixels() > p.pixels(),
                (Some(_), None) => true,
                _ => false,
            };
            target.still_beside =
                self.mode == Mode::Photo && larger && !self.no_beside.contains(&lens.id);
        }
        let settings = self.exposure.effective(&lens.caps);
        target.controls.af_continuous = lens.autofocus && settings.lens.is_auto();
        target.controls.settings = Some(settings);
        target.controls.frame_duration_us = self.frame_duration_for(&lens.id, purpose);
        if self.opts.transfer_exposure {
            if let Some((from, meta)) = &self.last_meta {
                let from_lens = self.table.get(*from);
                if let (Some(t), Some(f_old), Some(f_new)) =
                    (meta.exposure_us, from_lens.f_number, lens.f_number)
                {
                    let e = exposure::transfer(
                        exposure::Exposure {
                            time_us: t as f64,
                            gain: meta.analog_gain.unwrap_or(1.0) as f64,
                        },
                        f_old,
                        f_new,
                        1.0,
                        exposure::Limits::default(),
                    );
                    target.controls.exposure_us = Some(e.time_us.round() as i32);
                    target.controls.analog_gain = Some(e.gain as f32);
                }
            }
        }
        Some(target)
    }

    fn command(&self, action: Action) -> Command {
        match action {
            Action::Start { generation, target } => {
                let lens = self.table.get(target.lens);
                let stream = match target.purpose {
                    Purpose::Preview => StreamRequest {
                        purpose: Purpose::Preview,
                        size: if target.video {
                            lens.video
                        } else {
                            lens.preview
                        },
                        still: lens.still.filter(|_| target.still_beside),
                        still_buffers: self.opts.still_buffers,
                        video: target.video,
                        buffers: self.opts.preview_buffers,
                        fallbacks: Vec::new(),
                        raw: false,
                    },
                    Purpose::Still => {
                        let size = lens.still.or(lens.preview);
                        StreamRequest {
                            purpose: Purpose::Still,
                            size,
                            still: None,
                            still_buffers: 0,
                            video: false,
                            buffers: self.opts.still_buffers,
                            // The preview mode always fits: it just ran.
                            fallbacks: lens
                                .preview
                                .filter(|p| Some(*p) != size)
                                .into_iter()
                                .collect(),
                            // A burst merges the raw frames.
                            raw: self.opts.raw_merge && self.capture.burst().is_some(),
                        }
                    }
                };
                Command::Start {
                    generation,
                    camera: lens.id.clone(),
                    stream,
                    controls: target.controls,
                }
            }
            Action::Stop { generation } => Command::Stop { generation },
        }
    }

    fn run(&self, actions: Vec<Action>, out: &mut Vec<Output>) {
        out.extend(
            actions
                .into_iter()
                .map(|a| Output::Backend(self.command(a))),
        );
    }

    fn request(&mut self, target: Option<Target>, now: Instant, out: &mut Vec<Output>) {
        let actions = self.switch.request(target, now);
        self.run(actions, out);
    }

    fn request_preview(&mut self, now: Instant, out: &mut Vec<Output>) {
        if self.suspended {
            return;
        }
        // Another camera: metering, the EV slider and a lock start over.
        let next = self.side().map(|s| self.table.get(s.lens()).id.clone());
        if let (Some(wanted), Some(next)) = (self.switch.wanted(), next) {
            if wanted.camera != next {
                self.exposure.lens_switched();
                self.tap_point = None;
            }
        }
        let target = self.target(Purpose::Preview);
        self.request(target, now, out);
    }

    /// Start streaming (the app opened, or came back to the foreground).
    pub fn resume(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        // A stream starts afresh: focus and metering over the whole
        // picture, whatever was tapped before.
        self.forget_tap();
        self.suspended = false;
        self.retry_at = None;
        self.request_preview(now, &mut out);
        self.sync_light(&mut out);
        out
    }

    /// Stop streaming and let other apps have the cameras.
    pub fn suspend(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        self.suspended = true;
        self.recording = false;
        self.retry_at = None;
        self.capture.abort();
        self.abort_flash();
        self.request(None, now, &mut out);
        out.push(Output::Backend(Command::Release));
        self.sync_light(&mut out);
        out
    }

    pub fn set_zoom(&mut self, zoom: f64, source: ZoomSource, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        if self.busy() {
            return out;
        }
        let changed = self
            .side_mut()
            .and_then(|s| s.planner.set(zoom, source, now));
        if changed.is_some() {
            self.request_preview(now, &mut out);
        }
        out
    }

    /// A button of the lens row: its lens at its zoom, leaving the macro
    /// lens if that is on.
    pub fn press_zoom_button(&mut self, zoom: f64, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        if self.capture.is_busy() {
            return out;
        }
        let left_macro = self.facing == Facing::Back && self.macro_on && !self.recording;
        if left_macro {
            self.macro_on = false;
            self.focus.reset();
            self.last_meta = None;
        }
        let changed = self
            .side_mut()
            .and_then(|s| s.planner.set(zoom, ZoomSource::Preset, now));
        if changed.is_some() || left_macro {
            self.request_preview(now, &mut out);
        }
        out
    }

    /// Hold the chosen lens while zooming, or let the zoom pick it
    /// ([`Options::hold_lens`]).
    pub fn set_hold_lens(&mut self, on: bool, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        self.opts.hold_lens = on;
        let mut changed = false;
        for s in [&mut self.back, &mut self.front, &mut self.macro_side]
            .into_iter()
            .flatten()
        {
            changed |= s.planner.set_hold_lens(on).is_some();
        }
        if changed {
            self.request_preview(now, &mut out);
        }
        self.apply_low_light(now, &mut out);
        out
    }

    /// A pinch or slider drag ended.
    pub fn zoom_released(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        let changed = self.side_mut().and_then(|s| s.planner.release(now));
        if changed.is_some() {
            self.request_preview(now, &mut out);
        }
        out
    }

    /// Change whether taps focus (a setting).
    pub fn set_tap_to_focus(&mut self, on: bool) {
        self.opts.tap_to_focus = on;
    }

    /// Change the photo mode (a setting).
    pub fn set_capture_mode(&mut self, mode: capture::Mode) {
        self.opts.capture_mode = mode;
    }

    /// Photo or video. Ignored while recording or taking a photo. Video
    /// mode may run the camera faster (60 fps), and restarts the camera
    /// where the two modes stream differently (a video stream from the
    /// lens table, a still stream beside the photo preview).
    pub fn set_mode(&mut self, mode: Mode, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        if !self.recording && !self.busy() && self.mode != mode {
            self.mode = mode;
            // The stream restarts without the tapped window.
            self.forget_tap();
            self.request_preview(now, &mut out);
            self.push_frame_rate(&mut out);
        }
        out
    }

    /// The video frame rate setting (30 or 60). Ignored while recording.
    pub fn set_video_fps(&mut self, fps: u32) -> Vec<Output> {
        let mut out = Vec::new();
        if !self.recording && self.opts.video_fps != fps {
            self.opts.video_fps = fps;
            self.push_frame_rate(&mut out);
        }
        out
    }

    fn push_frame_rate(&self, out: &mut Vec<Output>) {
        let Some((generation, lens)) = self.preview_lens() else {
            return;
        };
        if let Some(d) = self.frame_duration_for(&lens.id, Purpose::Preview) {
            out.push(Output::Backend(Command::SetControls {
                generation,
                controls: crate::backend::Controls {
                    frame_duration_us: Some(d),
                    ..Default::default()
                },
            }));
        }
    }

    /// The Pro panel opened or closed: while it's open the zoom picks the
    /// lens strictly by the ladder (no low-light stand-in), since the user
    /// is choosing the camera's settings by hand.
    pub fn set_pro(&mut self, on: bool, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        self.pro = on;
        self.apply_low_light(now, &mut out);
        out
    }

    /// The low-light lens setting.
    pub fn set_low_light(&mut self, on: bool, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        self.opts.low_light = on;
        self.apply_low_light(now, &mut out);
        out
    }

    /// Bring the back side's fallback in line with the light, the
    /// settings and the Pro panel, and switch lenses if that changes
    /// which camera streams.
    fn apply_low_light(&mut self, now: Instant, out: &mut Vec<Output>) {
        let want =
            self.opts.low_light && !self.opts.hold_lens && !self.pro && self.low_light.is_dim();
        let Some(back) = self.back.as_mut() else {
            return;
        };
        if back.fallback == want {
            return;
        }
        let before = back.lens();
        back.fallback = want;
        let after = back.lens();
        if self.facing == Facing::Back && !self.macro_on && before != after && !self.busy() {
            self.request_preview(now, out);
        }
    }

    /// The camera that a recording would take its frames from: the
    /// streaming preview's, once its frames show.
    pub fn recording_source(&self) -> Option<String> {
        let (_, target) = self.switch.streaming()?;
        if target.purpose != Purpose::Preview || self.switch.is_switching() {
            return None;
        }
        Some(target.camera.clone())
    }

    /// Recording started or stopped (the app runs the encoder). While
    /// recording the lens doesn't change: zoom stays within it, and flip
    /// and photos wait.
    pub fn set_recording(&mut self, on: bool) {
        self.recording = on && self.mode == Mode::Video;
    }

    /// The lens streaming the preview, with its stream generation.
    fn preview_lens(&self) -> Option<(u64, crate::lens::Lens)> {
        let (generation, target) = self.switch.streaming()?;
        if target.purpose != Purpose::Preview || self.switch.is_switching() {
            return None;
        }
        let lens = self.table.lenses().get(target.lens).cloned()?;
        Some((generation, lens))
    }

    /// Send the current exposure settings to the streaming preview.
    fn push_settings(&self, out: &mut Vec<Output>) {
        let Some((generation, lens)) = self.preview_lens() else {
            return;
        };
        let settings = self.exposure.effective(&lens.caps);
        out.push(Output::Backend(Command::SetControls {
            generation,
            controls: crate::backend::Controls {
                af_continuous: lens.autofocus && settings.lens.is_auto() && !self.focus.is_active(),
                settings: Some(settings),
                ..Default::default()
            },
        }));
    }

    fn exposure_view(&self) -> ExposureView {
        let lens = self.side().map(|s| self.table.get(s.lens()));
        let caps = lens.map(|l| l.caps.clone()).unwrap_or_default();
        let base_iso = lens
            .map(|l| l.base_iso)
            .unwrap_or(crate::lens::DEFAULT_BASE_ISO);
        let meta = self.last_meta.as_ref().map(|(_, m)| m);
        ExposureView {
            settings: self.exposure.effective(&caps),
            pro: *self.exposure.pro(),
            ev_user: self.exposure.ev_user(),
            locked: self.exposure.is_locked(),
            base_iso,
            shutter_us: meta.and_then(|m| m.exposure_us),
            iso: meta.and_then(|m| {
                m.analog_gain.map(|g| {
                    exposure_control::iso(g as f64 * m.digital_gain.unwrap_or(1.0) as f64, base_iso)
                })
            }),
            lens_position: meta.and_then(|m| m.lens_position),
            caps,
        }
    }

    /// A tap on the drawn picture at (u, v), 0..1 from its top left, with
    /// the mean luma (0-255) of the tapped area when the app could measure
    /// it: focus there (unless focus is manual) and meter it; releases an
    /// AE/AF lock and resets the EV slider.
    pub fn tap(&mut self, u: f64, v: f64, luma: Option<f64>, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        if !self.opts.tap_to_focus || self.busy() {
            return out;
        }
        let Some((generation, lens)) = self.preview_lens() else {
            return out;
        };
        let Some(t) = self.transform(&lens.id) else {
            return out;
        };
        self.exposure.tap(luma, &lens.caps);
        self.tap_point = Some((u, v));
        let settings = self.exposure.effective(&lens.caps);
        // Spot metering meters the AfWindows rectangle: send it even when
        // focus is manual.
        let metering_window = match (settings.spot, lens.crop_maximum) {
            (true, Some(area)) => {
                let (x, y) = focus::picture_to_frame(u, v, &t);
                Some(focus::window(x, y, area))
            }
            _ => None,
        };
        // Video on a camera whose AF takes windows: continuous AF moves to
        // the tapped window and keeps following it, instead of a one-shot
        // scan that would hunt on camera and then hold.
        let follow = self.mode == Mode::Video
            && settings.lens.is_auto()
            && lens.autofocus
            && lens.af_windows
            && lens.crop_maximum.is_some();
        if follow {
            let area = lens.crop_maximum.expect("checked");
            let (x, y) = focus::picture_to_frame(u, v, &t);
            let window = focus::window(x, y, area);
            self.focus.follow(u, v, now);
            out.push(Output::Backend(Command::SetControls {
                generation,
                controls: crate::backend::Controls {
                    af_continuous: true,
                    af_window: Some(window),
                    settings: Some(settings),
                    metering_window,
                    ..Default::default()
                },
            }));
            return out;
        }
        let request = if settings.lens.is_auto() {
            self.focus.tap(
                u,
                v,
                &t,
                focus::Capability {
                    autofocus: lens.autofocus,
                    af_windows: lens.af_windows,
                    area: lens.crop_maximum,
                },
                now,
            )
        } else {
            None
        };
        out.push(Output::Backend(Command::SetControls {
            generation,
            controls: crate::backend::Controls {
                focus: request,
                settings: Some(settings),
                metering_window,
                ..Default::default()
            },
        }));
        out
    }

    /// Forget the tapped point (its focus ring included): focus and
    /// metering over the whole picture.
    fn forget_tap(&mut self) {
        self.tap_point = None;
        self.exposure.clear_tap();
        self.focus.reset();
    }

    /// Give up the tapped point: focus and metering over the whole picture
    /// again, as before the tap (a tap on its marker, or the app coming
    /// back to the front).
    pub fn clear_tap(&mut self) -> Vec<Output> {
        let mut out = Vec::new();
        if self.capture.is_busy() {
            return out;
        }
        self.forget_tap();
        let Some((generation, lens)) = self.preview_lens() else {
            return out;
        };
        let settings = self.exposure.effective(&lens.caps);
        out.push(Output::Backend(Command::SetControls {
            generation,
            controls: crate::backend::Controls {
                af_continuous: lens.autofocus && settings.lens.is_auto(),
                settings: Some(settings),
                whole_frame_af: true,
                ..Default::default()
            },
        }));
        out
    }

    /// One autofocus scan where the AF looks, keeping the exposure as it
    /// is (as recording starts).
    pub fn refocus(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        let Some((generation, lens)) = self.preview_lens() else {
            return out;
        };
        if !self.exposure.effective(&lens.caps).lens.is_auto() {
            return out;
        }
        let Some(t) = self.transform(&lens.id) else {
            return out;
        };
        let cap = focus::Capability {
            autofocus: lens.autofocus,
            af_windows: false,
            area: None,
        };
        if let Some(request) = self.focus.tap(0.5, 0.5, &t, cap, now) {
            out.push(Output::Backend(Command::SetControls {
                generation,
                controls: crate::backend::Controls {
                    focus: Some(request),
                    ..Default::default()
                },
            }));
        }
        out
    }

    /// A long press: AE/AF lock at what the camera reports now.
    pub fn lock(&mut self, now: Instant) -> Vec<Output> {
        let _ = now;
        let mut out = Vec::new();
        let Some((_, lens)) = self.preview_lens() else {
            return out;
        };
        let meta = self.last_meta.as_ref().map(|(_, m)| m.clone());
        if self.exposure.lock(meta.as_ref(), &lens.caps) {
            self.focus.reset();
            self.push_settings(&mut out);
        } else {
            out.push(Output::Error(
                "This camera doesn't offer manual exposure yet, so it can't lock".into(),
            ));
        }
        out
    }

    pub fn unlock(&mut self) -> Vec<Output> {
        let mut out = Vec::new();
        if self.exposure.is_locked() {
            self.exposure.unlock();
            self.push_settings(&mut out);
        }
        out
    }

    /// The EV slider.
    pub fn set_ev(&mut self, ev: f32) -> Vec<Output> {
        let mut out = Vec::new();
        let caps = self.exposure_view().caps;
        self.exposure.set_ev(ev, &caps);
        self.push_settings(&mut out);
        out
    }

    pub fn set_shutter(&mut self, s: Setting<i32>) -> Vec<Output> {
        self.exposure.set_shutter(s);
        let mut out = Vec::new();
        self.push_settings(&mut out);
        out
    }

    /// ISO, as the Pro panel shows it; sent as analog gain.
    pub fn set_iso(&mut self, iso: Setting<u32>) -> Vec<Output> {
        let base = self.exposure_view().base_iso;
        self.exposure.set_gain(match iso {
            Setting::Auto => Setting::Auto,
            Setting::Manual(i) => Setting::Manual(exposure_control::gain_for_iso(i, base)),
        });
        let mut out = Vec::new();
        self.push_settings(&mut out);
        out
    }

    pub fn set_white_balance(&mut self, s: Setting<Awb>) -> Vec<Output> {
        self.exposure.set_white_balance(s);
        let mut out = Vec::new();
        self.push_settings(&mut out);
        out
    }

    pub fn set_lens_position(&mut self, s: Setting<f32>) -> Vec<Output> {
        self.exposure.set_lens(s);
        self.focus.reset();
        let mut out = Vec::new();
        self.push_settings(&mut out);
        out
    }

    /// Everything back to automatic, a tapped point included.
    pub fn reset_exposure(&mut self) -> Vec<Output> {
        self.exposure.reset();
        let mut out = self.clear_tap();
        if out.is_empty() {
            self.push_settings(&mut out);
        }
        out
    }

    /// Choose the side to open on, before [`Controller::resume`]. Ignored
    /// when that side has no camera.
    pub fn set_facing(&mut self, facing: Facing) {
        let has = match facing {
            Facing::Back => self.back.is_some(),
            Facing::Front => self.front.is_some(),
        };
        if has {
            self.facing = facing;
        }
    }

    pub fn flip(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        if self.recording || self.busy() || self.back.is_none() || self.front.is_none() {
            return out;
        }
        self.facing = match self.facing {
            Facing::Back => Facing::Front,
            Facing::Front => Facing::Back,
        };
        // The back side comes back with its zoom ladder.
        self.macro_on = false;
        // Exposure doesn't carry between the two sides.
        self.last_meta = None;
        self.request_preview(now, &mut out);
        self.sync_light(&mut out);
        out
    }

    /// Turn the macro lens on or off: on, it streams for the back side
    /// instead of the zoom ladder's lens, at its own zoom. Not while
    /// recording or taking a photo, nor on the front side.
    pub fn toggle_macro(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        if self.recording || self.busy() || self.facing != Facing::Back || self.macro_side.is_none()
        {
            return out;
        }
        self.macro_on = !self.macro_on;
        // Another lens, nothing to carry over from the old one's focus.
        self.focus.reset();
        self.last_meta = None;
        self.request_preview(now, &mut out);
        self.sync_light(&mut out);
        out
    }

    pub fn shutter(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        if self.recording
            || self.pre_flash.is_some()
            || self.switch.streaming().is_none()
            || self.switch.is_switching()
            || self.side().is_none()
        {
            return out;
        }
        if self.light_out_photo {
            return out;
        }
        if !self.capture.is_busy() && self.flash_fires() {
            self.light_out = None;
            // Light first; the photo once the picture has settled under it.
            let demand = self
                .last_meta
                .as_ref()
                .and_then(|(_, m)| crate::flash::FlashSpec::demand(m));
            tracing::info!(
                mode = self.opts.flash_mode.name(),
                demand = demand.map(|d| format!("{d:.0}")),
                "flash: light on for the photo"
            );
            self.pre_flash = Some(PreFlash::new(now, self.last_meta.as_ref().map(|(_, m)| m)));
            self.flash_photo = true;
            self.sync_light(&mut out);
            return out;
        }
        if !self.capture.is_busy() {
            // The torch just went out: the photo once the exposure has
            // walked back from it.
            if let Some(LightOut::Pending(before)) = &self.light_out {
                self.light_out = Some(LightOut::Waiting(PreFlash::light_out(now, before.as_ref())));
            }
            if self.light_out.is_some() {
                tracing::info!("flash: the photo waits for the exposure after the torch");
                self.light_out_photo = true;
                return out;
            }
        }
        self.begin_capture(now, &mut out);
        self.sync_light(&mut out);
        out
    }

    /// Start taking the photo: the capture mode the lens and its stream
    /// allow.
    fn begin_capture(&mut self, now: Instant, out: &mut Vec<Output>) {
        let Some(lens) = self.side().map(|s| s.lens()) else {
            return;
        };
        let l = self.table.get(lens);
        let autofocus = l.autofocus;
        // A still mode no bigger than the preview's gains nothing from a
        // stop and a start: take the next preview frame.
        let same_mode = l.still.is_none() || l.still == l.preview;
        let beside = self
            .switch
            .streaming()
            .is_some_and(|(g, _)| self.beside.is_some_and(|(b, _)| b == g));
        let wanted = match self.opts.capture_mode {
            capture::Mode::Burst(_) if !self.table.bursts() => {
                tracing::info!(
                    "no burst: the camera merges its own frames for each photo; a single photo instead"
                );
                capture::Mode::Full
            }
            m => m,
        };
        let mode = if same_mode {
            capture::Mode::Quick
        } else if beside && wanted == capture::Mode::Full {
            capture::Mode::Beside
        } else {
            wanted
        };
        let meta = self.last_meta.as_ref().map(|(_, m)| m);
        let step = self.capture.begin(mode, autofocus, meta, now);
        self.capture_step(step, now, out);
    }

    /// Busy with a photo: taking it, or waiting under the flash's light.
    fn busy(&self) -> bool {
        self.capture.is_busy() || self.pre_flash.is_some() || self.light_out_photo
    }

    /// Whether the next photo lights the flash first: on, or auto in a
    /// dim scene; on a back lens, in photo mode. (The torch is lit
    /// already.)
    fn flash_fires(&self) -> bool {
        if !self.opts.flash || self.facing != Facing::Back || self.mode != Mode::Photo {
            return false;
        }
        match self.opts.flash_mode {
            FlashMode::On => true,
            FlashMode::Auto => self
                .table
                .flash()
                .auto_fires(self.last_meta.as_ref().map(|(_, m)| m)),
            FlashMode::Off | FlashMode::Torch => false,
        }
    }

    /// Forget a flash photo (the stream stopped or broke).
    fn abort_flash(&mut self) {
        self.pre_flash = None;
        self.flash_photo = false;
        self.light_out = None;
        self.light_out_photo = false;
    }

    /// What the LED should do now.
    fn wanted_light(&self) -> Light {
        if !self.opts.flash || self.suspended || self.facing != Facing::Back {
            return Light::Off;
        }
        // The torch comes on once a back lens streams and stays on through
        // lens switches; it goes out when nothing is wanted (put away, or
        // the camera failed).
        let torch = self.opts.flash_mode == FlashMode::Torch
            && self.switch.wanted().is_some()
            && (self.switch.streaming().is_some() || self.light != Light::Off);
        if self.flash_photo {
            Light::Photo
        } else if torch {
            Light::Torch
        } else {
            Light::Off
        }
    }

    /// Bring the LED in line: the photo's light ends with the photo.
    fn sync_light(&mut self, out: &mut Vec<Output>) {
        if self.flash_photo && self.pre_flash.is_none() && !self.capture.is_busy() {
            self.flash_photo = false;
        }
        let want = self.wanted_light();
        if want != self.light {
            // The torch out while the back camera keeps streaming: the
            // next photo waits for the exposure (not the photo's light,
            // which ends with its photo, nor when the stream stops).
            let torch_out = self.light == Light::Torch
                && want == Light::Off
                && !self.suspended
                && !self.flash_photo
                && self.facing == Facing::Back
                && self.switch.streaming().is_some();
            if torch_out {
                let before = self.last_meta.as_ref().map(|(_, m)| m.clone());
                self.light_out = Some(LightOut::Pending(before));
            }
            tracing::info!(light = ?want, "flash");
            self.light = want;
            out.push(Output::Light(want));
        }
    }

    /// A preview frame after the torch went out: the wait for the exposure
    /// starts, or goes on, or ends (and a photo waiting for it is taken).
    fn light_out_frame(&mut self, meta: &FrameMeta, now: Instant, out: &mut Vec<Output>) {
        let step = match &mut self.light_out {
            None => return,
            Some(LightOut::Pending(before)) => {
                let mut p = PreFlash::light_out(now, before.as_ref());
                let step = p.on_frame(meta, now);
                self.light_out = Some(LightOut::Waiting(p));
                step
            }
            Some(LightOut::Waiting(p)) => p.on_frame(meta, now),
        };
        if step == PreFlashStep::Wait {
            return;
        }
        if let Some(LightOut::Waiting(p)) = &self.light_out {
            tracing::info!(
                frames = p.frames(),
                ms = p.elapsed(now).as_millis() as u64,
                settled = step == PreFlashStep::Settled,
                exposure_us = meta.exposure_us,
                gain = meta.analog_gain,
                photo = self.light_out_photo,
                "flash: the exposure after the torch"
            );
        }
        self.light_out = None;
        if std::mem::take(&mut self.light_out_photo) {
            self.begin_capture(now, out);
        }
    }

    /// What the flash does: off, auto, on, or the torch. Kept on the
    /// front camera (where nothing lights) for when the back comes back.
    /// A flash photo under way finishes with its light.
    pub fn set_flash_mode(&mut self, mode: FlashMode) -> Vec<Output> {
        let mut out = Vec::new();
        self.opts.flash_mode = mode;
        self.sync_light(&mut out);
        out
    }

    pub fn flash_mode(&self) -> FlashMode {
        self.opts.flash_mode
    }

    fn capture_step(&mut self, step: Step, now: Instant, out: &mut Vec<Output>) {
        match step {
            Step::StartStill(controls) => {
                if let Some(mut target) = self.target(Purpose::Still) {
                    // The still keeps the exposure settings; the locked
                    // values are start hints.
                    target.controls.exposure_us = controls.exposure_us;
                    target.controls.analog_gain = controls.analog_gain;
                    target.controls.color_gains = controls.color_gains;
                    target.controls.lens_position = controls.lens_position;
                    target.controls.af_continuous = false;
                    self.request(Some(target), now, out);
                }
            }
            Step::Capture => {
                if let Some((generation, _)) = self.switch.streaming() {
                    out.push(Output::Backend(Command::Capture { generation }));
                }
            }
            Step::Fail(msg) => {
                out.push(Output::Error(format!("Photo failed: {msg}")));
                self.restore_preview(now, out);
            }
            Step::Apply(controls) => {
                // A burst's lock: hold its exposure, gain, color and focus
                // (manual settings hold; the values in `controls` alone are
                // only start hints the AGC would walk away from).
                if let Some((generation, target)) = self.switch.streaming() {
                    let caps = self.table.get(target.lens).caps.clone();
                    let mut settings = self.exposure.effective(&caps);
                    if let Some(t) = controls.exposure_us {
                        settings.shutter = Setting::Manual(t);
                    }
                    if let Some(g) = controls.analog_gain {
                        settings.gain = Setting::Manual(g);
                    }
                    if let Some(gains) = controls.color_gains {
                        settings.white_balance = Setting::Manual(Awb::Gains(gains));
                    }
                    if let Some(p) = controls.lens_position {
                        settings.lens = Setting::Manual(p);
                    }
                    out.push(Output::Backend(Command::SetControls {
                        generation,
                        controls: crate::backend::Controls {
                            settings: Some(settings),
                            ..controls
                        },
                    }));
                }
            }
            Step::Unlock => {
                if let Some((generation, target)) = self.switch.streaming() {
                    let caps = self.table.get(target.lens).caps.clone();
                    out.push(Output::Backend(Command::SetControls {
                        generation,
                        controls: crate::backend::Controls {
                            settings: Some(self.exposure.effective(&caps)),
                            ..Default::default()
                        },
                    }));
                }
            }
            Step::Nothing
            | Step::Keep
            | Step::KeepBurst
            | Step::KeepAndRestore(_)
            | Step::Discard => {}
        }
    }

    fn restore_preview(&mut self, now: Instant, out: &mut Vec<Output>) {
        if let Some(mut target) = self.target(Purpose::Preview) {
            let locked = self.capture.locked().clone();
            target.controls.exposure_us = locked.exposure_us;
            target.controls.analog_gain = locked.analog_gain;
            self.request(Some(target), now, out);
        }
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        if self.capture.is_busy() {
            let meta = self.last_meta.as_ref().map(|(_, m)| m.clone());
            let step = self.capture.tick(meta.as_ref(), now);
            self.capture_step(step, now, &mut out);
        } else if let Some(p) = &self.pre_flash {
            if p.tick(now) != PreFlashStep::Wait {
                tracing::info!(
                    frames = p.frames(),
                    reacted = p.reacted(),
                    "flash: the wait under the light ran out; the photo anyway"
                );
                self.pre_flash = None;
                self.begin_capture(now, &mut out);
            }
        } else if let Some(LightOut::Waiting(p)) = &self.light_out {
            if p.tick(now) != PreFlashStep::Wait {
                tracing::info!(
                    frames = p.frames(),
                    "flash: the wait after the torch ran out; the photo anyway"
                );
                self.light_out = None;
                if std::mem::take(&mut self.light_out_photo) {
                    self.begin_capture(now, &mut out);
                }
            }
        } else if self.side_mut().and_then(|s| s.planner.tick(now)).is_some() {
            self.request_preview(now, &mut out);
        }
        if self.focus.tick(now) == focus::Action::ResumeContinuous {
            if let Some((generation, target)) = self.switch.streaming() {
                let af = self
                    .table
                    .lenses()
                    .get(target.lens)
                    .is_some_and(|l| l.autofocus);
                let auto = self
                    .table
                    .lenses()
                    .get(target.lens)
                    .is_some_and(|l| self.exposure.effective(&l.caps).lens.is_auto());
                if af && auto && target.purpose == Purpose::Preview {
                    out.push(Output::Backend(Command::SetControls {
                        generation,
                        controls: crate::backend::Controls {
                            af_continuous: true,
                            ..Default::default()
                        },
                    }));
                }
            }
        }
        if self.retry_at.is_some_and(|t| now >= t) {
            self.retry_at = None;
            self.request_preview(now, &mut out);
        }
        self.sync_light(&mut out);
        out
    }

    fn picture(&self, frame: Frame, lens_idx: usize, purpose: Purpose) -> Output {
        let lens = self.table.get(lens_idx).clone();
        let zoom = self.side().map(|s| s.planner.zoom()).unwrap_or(1.0);
        let transform = self.transform_for(&lens.id, purpose).unwrap_or_default();
        Output::Picture(Box::new(Picture {
            transform,
            crop: (zoom / lens.zoom).max(1.0),
            rotation: lens.upright_clockwise(),
            zoom,
            settings: self.exposure.effective(&lens.caps),
            flash: crate::flash::exif_flash(
                self.opts.flash && lens.role != Role::Front,
                self.opts.flash_mode,
                self.flash_photo || self.light != Light::Off,
            ),
            lens,
            frame,
        }))
    }

    /// Judge the light from a back lens's preview frame. Not while
    /// recording (the lens doesn't change under a recording for the
    /// light) or taking a photo.
    fn judge_light(
        &mut self,
        lens_idx: usize,
        meta: &FrameMeta,
        now: Instant,
        out: &mut Vec<Output>,
    ) {
        if self.facing != Facing::Back
            || self.macro_on
            || self.recording
            || self.busy()
            || self.switch.is_switching()
        {
            return;
        }
        let Some(tele) = self.table.by_role(Role::Tele).map(|i| self.table.get(i)) else {
            return;
        };
        let lens = self.table.get(lens_idx);
        let (Some(f), Some(f_tele)) = (lens.f_number, tele.f_number) else {
            return;
        };
        let demand = lowlight::tele_demand(meta, f, f_tele);
        let max_gain = tele.caps.gain.map(|(_, hi)| hi as f64);
        if self.low_light.update(demand, max_gain, now).is_some() {
            tracing::info!(dim = self.low_light.is_dim(), "low light");
            self.apply_low_light(now, out);
        }
    }

    pub fn on_event(&mut self, event: Event, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        match event {
            Event::Cameras(_) => {}
            Event::Notice { message } => out.push(Output::Error(message)),
            Event::Started {
                generation,
                ref camera,
                max_fps,
                min_fps,
                still,
                ..
            } => {
                match max_fps {
                    Some(f) => {
                        self.frame_rates.insert(camera.clone(), (f, min_fps));
                    }
                    None => {
                        self.frame_rates.remove(camera);
                    }
                }
                self.focus.reset();
                let actions = self.switch.on_started(generation);
                self.beside = still.map(|s| (generation, s));
                let refused = still.is_none()
                    && self
                        .switch
                        .streaming()
                        .is_some_and(|(g, t)| g == generation && t.still_beside);
                if refused {
                    // Don't ask this camera again, and don't restart it
                    // for having said no.
                    tracing::info!(camera, "no still stream beside the preview");
                    self.no_beside.insert(camera.clone());
                    self.switch.amend(generation, |t| t.still_beside = false);
                }
                self.run(actions, &mut out);
            }
            Event::Stopped { generation } => {
                let actions = self.switch.on_stopped(generation);
                self.run(actions, &mut out);
            }
            Event::Error {
                generation,
                message,
            } => {
                let actions = self.switch.on_error(generation);
                self.run(actions, &mut out);
                self.capture.abort();
                self.abort_flash();
                out.push(Output::Error(message));
                if !self.suspended && self.switch.wanted().is_none() {
                    self.retry_at = Some(now + RETRY_AFTER);
                }
            }
            Event::Frame(frame) => self.on_frame(frame, now, &mut out),
        }
        self.sync_light(&mut out);
        out
    }

    fn on_frame(&mut self, frame: Frame, now: Instant, out: &mut Vec<Output>) {
        if self.switch.on_frame(frame.generation, now) == FrameKind::Stale {
            return;
        }
        let Some((_, target)) = self.switch.streaming() else {
            return;
        };
        let (lens_idx, purpose) = (target.lens, target.purpose);
        if lens_idx >= self.table.lenses().len() {
            return; // its camera is gone
        }
        if frame.still {
            // From the still stream beside the preview.
            if self.capture.on_beside_frame(&frame.meta, now) == Step::Keep {
                if let Some(r) = self.capture.last_still() {
                    let m = &r.meta;
                    tracing::info!(
                        "still from {} beside the preview: {} ms after asking, exposure {} us, \
                         analog gain {}, AE {}, lens {}",
                        self.table.get(lens_idx).name,
                        r.elapsed.as_millis(),
                        opt(m.exposure_us),
                        opt(m.analog_gain.map(|g| format!("{g:.2}"))),
                        opt(m.ae_state.map(|s| format!("{s:?}"))),
                        opt(m.lens_position.map(|p| format!("{p:.3}"))),
                    );
                }
                out.push(self.picture(frame, lens_idx, Purpose::Still));
            }
            return;
        }
        match purpose {
            Purpose::Preview => {
                self.focus.on_meta(&frame.meta, now);
                self.judge_light(lens_idx, &frame.meta, now, out);
                self.last_meta = Some((lens_idx, frame.meta.clone()));
                if let Some(p) = &mut self.pre_flash {
                    let step = p.on_frame(&frame.meta, now);
                    tracing::debug!(
                        ms = p.elapsed(now).as_millis() as u64,
                        exposure_us = frame.meta.exposure_us,
                        gain = frame.meta.analog_gain,
                        digital = frame.meta.digital_gain,
                        ae = ?frame.meta.ae_state,
                        gains = ?frame.meta.color_gains,
                        reacted = p.reacted(),
                        ?step,
                        "flash: frame under the light"
                    );
                    if step != PreFlashStep::Wait {
                        tracing::info!(
                            frames = p.frames(),
                            ms = p.elapsed(now).as_millis() as u64,
                            settled = step == PreFlashStep::Settled,
                            reacted = p.reacted(),
                            exposure_us = frame.meta.exposure_us,
                            gain = frame.meta.analog_gain,
                            "flash: the picture under the light"
                        );
                        self.pre_flash = None;
                        self.begin_capture(now, out);
                    }
                }
                self.light_out_frame(&frame.meta, now, out);
                match self.capture.on_preview_frame(&frame.meta, now) {
                    Step::Keep => out.push(self.picture(frame, lens_idx, purpose)),
                    step => {
                        self.capture_step(step, now, out);
                        out.push(Output::Show(frame));
                    }
                }
            }
            Purpose::Still => match self.capture.on_still_frame(&frame.meta, now) {
                Step::KeepAndRestore(_) => {
                    if let Some(r) = self.capture.last_still() {
                        let m = &r.meta;
                        let lens = &self.table.get(lens_idx).name;
                        tracing::info!(
                            "still from {lens}: frame {} after {} ms ({}), exposure {} us, \
                             analog gain {}, digital gain {}, AE {}",
                            r.frames,
                            r.elapsed.as_millis(),
                            if r.settled { "settled" } else { "wait ran out" },
                            opt(m.exposure_us),
                            opt(m.analog_gain.map(|g| format!("{g:.2}"))),
                            opt(m.digital_gain.map(|g| format!("{g:.2}"))),
                            opt(m.ae_state.map(|s| format!("{s:?}"))),
                        );
                    }
                    if self.capture.burst().is_some() {
                        out.push(burst_frame(self.picture(frame, lens_idx, purpose)));
                        out.push(Output::BurstDone);
                    } else {
                        out.push(self.picture(frame, lens_idx, purpose));
                    }
                    self.restore_preview(now, out);
                }
                Step::KeepBurst => out.push(burst_frame(self.picture(frame, lens_idx, purpose))),
                Step::Discard => {}
                step => self.capture_step(step, now, out),
            },
        }
    }
}

/// A picture output as one frame of a burst.
fn burst_frame(o: Output) -> Output {
    match o {
        Output::Picture(p) => Output::BurstFrame(p),
        other => other,
    }
}

/// A value for a log line, or "-".
fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{FrameData, Location, PixelLayout, Size};
    use crate::lens::TableSpec;

    fn cameras() -> Vec<CameraInfo> {
        let cam = |id: &str, location, rotation| CameraInfo {
            id: id.into(),
            location,
            rotation,
            model: "sensor".into(),
            autofocus: true,
            ..CameraInfo::default()
        };
        vec![
            cam("main", Location::Back, 90),
            cam("uw", Location::Back, 270),
            cam("tele", Location::Back, 90),
            cam("front", Location::Front, 90),
        ]
    }

    const SPEC: &str = r#"
max_zoom = 15.0
[[lens]]
role = "main"
id = "main"
zoom = 1.0
f_number = 1.85
preview = [2016, 1136]
still = [4080, 3072]
autofocus = true
[[lens]]
role = "ultrawide"
id = "uw"
zoom = 0.45
f_number = 2.2
preview = [2016, 1508]
[[lens]]
role = "tele"
id = "tele"
zoom = 4.78
f_number = 3.5
preview = [2016, 1512]
[[lens]]
role = "front"
id = "front"
zoom = 1.0
mirror = true
"#;

    fn spec() -> TableSpec {
        TableSpec::parse(SPEC).unwrap()
    }

    fn commands(out: &[Output]) -> Vec<&Command> {
        out.iter()
            .filter_map(|o| match o {
                Output::Backend(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    fn start_of(out: &[Output]) -> (u64, String, Purpose) {
        match commands(out).as_slice() {
            [Command::Start {
                generation,
                camera,
                stream,
                ..
            }] => (*generation, camera.clone(), stream.purpose),
            other => panic!("expected one start, got {other:?}"),
        }
    }

    fn frame(generation: u64, camera: &str, exposure: i32) -> Event {
        Event::Frame(Frame {
            generation,
            camera: camera.into(),
            size: Size::new(4, 2),
            stride: 16,
            layout: PixelLayout::Xrgb8888,
            data: FrameData::Memory(vec![0; 32]),
            meta: FrameMeta {
                exposure_us: Some(exposure),
                analog_gain: Some(1.0),
                ..FrameMeta::default()
            },
            raw: None,
            still: false,
        })
    }

    fn started(generation: u64, camera: &str) -> Event {
        Event::Started {
            generation,
            camera: camera.into(),
            size: Size::new(4, 2),
            stride: 16,
            layout: PixelLayout::Xrgb8888,
            max_fps: None,
            min_fps: None,
            still: None,
        }
    }

    /// Opens the app on the main camera and returns its generation.
    fn open(c: &mut Controller, now: Instant) -> u64 {
        let (g, cam, _) = start_of(&c.resume(now));
        assert_eq!(cam, "main");
        c.on_event(started(g, "main"), now);
        c.on_event(frame(g, "main", 10_000), now);
        g
    }

    #[test]
    fn opens_on_the_main_camera_at_1x() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let out = c.resume(now);
        match commands(&out).as_slice() {
            [Command::Start {
                camera,
                stream,
                controls,
                ..
            }] => {
                assert_eq!(camera, "main");
                assert_eq!(stream.size, Some(Size::new(2016, 1136)));
                assert_eq!(stream.buffers, 4);
                assert!(controls.af_continuous);
            }
            other => panic!("{other:?}"),
        }
        let v = c.view();
        assert_eq!(v.zoom, 1.0);
        assert_eq!(
            v.presets,
            vec![0.45, 1.0, 2.0, 5.0],
            "the .5 button is the full ultrawide"
        );
        assert!(v.can_flip);
    }

    #[test]
    fn preset_5x_switches_to_the_tele_with_exposure_carried() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let g = open(&mut c, now);
        let out = c.set_zoom(5.0, ZoomSource::Preset, now);
        match commands(&out).as_slice() {
            [Command::Start {
                generation,
                camera,
                controls,
                ..
            }] => {
                assert!(*generation > g);
                assert_eq!(camera, "tele");
                // 10 ms at f/1.85 needs 35.8 ms at f/3.5: time capped at
                // 33.3 ms, the rest in gain.
                assert_eq!(controls.exposure_us, Some(33_333));
                assert!(controls.analog_gain.unwrap() > 1.0);
                assert!(!controls.af_continuous, "the tele has no AF");
            }
            other => panic!("{other:?}"),
        }
        assert!(c.view().switching);
        // The main's frames still arriving are dropped.
        assert!(c.on_event(frame(g, "main", 10_000), now).is_empty());
        let t = c.transform("tele").unwrap();
        assert!((t.scale - 5.0 / 4.78).abs() < 1e-9);
        // The frozen main frame is drawn at 5x meanwhile.
        assert_eq!(c.transform("main").unwrap().scale, 5.0);
    }

    #[test]
    fn a_pinch_switches_after_it_settles() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        assert!(commands(&c.set_zoom(0.6, ZoomSource::Gesture, now)).is_empty());
        assert!(c.next_tick(now).is_some());
        assert!(commands(&c.tick(now + Duration::from_millis(50))).is_empty());
        let (_, cam, _) = start_of(&c.tick(now + Duration::from_millis(160)));
        assert_eq!(cam, "uw");
    }

    #[test]
    fn transforms_turn_each_camera_upright() {
        // main 90, uw 270, tele 90 (this test's table), front 90: turn
        // counter-clockwise by Rotation, so clockwise by 360 - Rotation.
        let c = Controller::new(&spec(), &cameras(), Options::default());
        assert_eq!(c.transform("main").unwrap().rotation, 270);
        assert_eq!(c.transform("uw").unwrap().rotation, 90);
        assert_eq!(c.transform("tele").unwrap().rotation, 270);
        let front = c.transform("front").unwrap();
        assert_eq!((front.rotation, front.mirror), (270, true));
    }

    #[test]
    fn frames_are_never_drawn_below_their_own_field_of_view() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        let mut z = 0.45;
        while z <= 10.0 {
            c.set_zoom(z, ZoomSource::Gesture, now);
            for cam in ["main", "uw", "tele"] {
                let t = c.transform(cam).unwrap();
                assert!(t.scale >= 1.0, "{cam} at {z}: scale {}", t.scale);
            }
            // The lens that should be streaming crops exactly zoom/factor.
            let lens = c.table().get(c.side().unwrap().lens());
            let t = c.transform(&lens.id).unwrap();
            assert!((t.scale - (z / lens.zoom).max(1.0)).abs() < 1e-9);
            z += 0.01;
        }
        // The frozen main frame while the ultrawide starts for 0.6x: full
        // view, not shrunk.
        c.set_zoom(0.6, ZoomSource::Preset, now);
        assert_eq!(c.transform("main").unwrap().scale, 1.0);
        assert!((c.transform("uw").unwrap().scale - 0.6 / 0.45).abs() < 1e-9);
    }

    #[test]
    fn a_still_in_the_preview_mode_reuses_the_running_stream() {
        // The ultrawide's still mode is its preview mode (the full one
        // waits for the kernel): no stop, no start, the next frame.
        let now = Instant::now();
        let spec = TableSpec::parse(
            "[[lens]]\nrole = \"main\"\nid = \"main\"\nzoom = 1.0\npreview = [2016, 1508]\nstill = [2016, 1508]\n",
        )
        .unwrap();
        let mut c = Controller::new(&spec, &cameras(), Options::default());
        let g = open(&mut c, now);
        assert!(commands(&c.shutter(now)).is_empty(), "no restart");
        match c.on_event(frame(g, "main", 10_000), now).as_slice() {
            [Output::Picture(p)] => assert_eq!(p.crop, 1.0),
            other => panic!("{other:?}"),
        }
        assert!(!c.view().capturing);
    }

    #[test]
    fn the_half_button_shows_the_whole_ultrawide() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        let half = c.view().presets[0];
        c.set_zoom(half, ZoomSource::Preset, now);
        assert_eq!(c.transform("uw").unwrap().scale, 1.0);
    }

    #[test]
    fn a_tap_focuses_the_streaming_lens_and_continuous_af_returns() {
        let now = Instant::now();
        let mut cams = cameras();
        for c in cams.iter_mut() {
            c.af_windows = true;
            c.crop_maximum = Some(crate::backend::Rect {
                x: 0,
                y: 0,
                width: 4080,
                height: 3072,
            });
        }
        let mut c = Controller::new(&spec(), &cams, Options::default());
        let g = open(&mut c, now);
        match commands(&c.tap(0.25, 0.25, None, now)).as_slice() {
            [Command::SetControls {
                generation,
                controls,
            }] => {
                assert_eq!(*generation, g);
                assert!(controls.focus.unwrap().window.is_some());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(c.view().focus.unwrap().status, focus::Status::Scanning);
        let later = now + focus::SCAN_TIMEOUT + focus::NOTICE;
        c.tick(now + focus::SCAN_TIMEOUT);
        let out = c.tick(later);
        assert!(matches!(
            commands(&out).as_slice(),
            [Command::SetControls { controls, .. }] if controls.af_continuous
        ));
    }

    #[test]
    fn taps_on_a_fixed_focus_lens_meter_only_and_off_send_nothing() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        c.set_zoom(5.0, ZoomSource::Preset, now);
        let (g, _, _) = (2, "tele", Purpose::Preview);
        c.on_event(started(g, "tele"), now);
        c.on_event(frame(g, "tele", 10_000), now);
        match commands(&c.tap(0.5, 0.5, None, now)).as_slice() {
            [Command::SetControls { controls, .. }] => assert!(controls.focus.is_none()),
            other => panic!("{other:?}"),
        }
        assert_eq!(c.view().focus.unwrap().status, focus::Status::FixedFocus);
        c.set_tap_to_focus(false);
        c.set_zoom(1.0, ZoomSource::Preset, now);
        assert!(c.tap(0.5, 0.5, None, now).is_empty());
    }

    #[test]
    fn cameras_that_appear_late_start_and_a_removed_one_stops() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &[], Options::default());
        assert!(!c.has_cameras());
        assert!(commands(&c.resume(now)).is_empty(), "nothing to start yet");
        assert!(c.is_running());
        // The drivers load: the main camera starts.
        let (g, cam, _) = start_of(&c.set_cameras(&spec(), &cameras(), now));
        assert_eq!(cam, "main");
        c.on_event(started(g, "main"), now);
        c.on_event(frame(g, "main", 10_000), now);
        // The main camera goes away: its stream stops; the ultrawide is
        // what's left at 1x on the back.
        let mut left = cameras();
        left.retain(|c| c.id != "main");
        // A start replaces the old stream; generations carry on.
        let (g2, cam, _) = start_of(&c.set_cameras(&spec(), &left, now));
        assert!(g2 > g, "generations carry on");
        assert_eq!(cam, "uw");
        // The backend reports the main stream stopped (its camera went):
        // nothing more to do.
        assert!(c.on_event(Event::Stopped { generation: g }, now).is_empty());
        // All gone: stopped, nothing to start, still running (waiting).
        c.on_event(started(g2, "uw"), now);
        let out = c.set_cameras(&spec(), &[], now);
        assert_eq!(commands(&out), vec![&Command::Stop { generation: g2 }]);
        assert!(!c.has_cameras());
        assert!(c
            .on_event(Event::Stopped { generation: g2 }, now)
            .is_empty());
    }

    #[test]
    fn a_new_camera_leaves_the_streaming_one_alone() {
        let now = Instant::now();
        let mut cams = cameras();
        let tele = cams.pop().unwrap(); // front
        let front = tele;
        let mut c = Controller::new(&spec(), &cams, Options::default());
        let g = open(&mut c, now);
        cams.push(front);
        assert!(
            commands(&c.set_cameras(&spec(), &cams, now)).is_empty(),
            "main keeps streaming"
        );
        assert_eq!(
            c.on_event(frame(g, "main", 10_000), now).len(),
            1,
            "its frames still show"
        );
    }

    #[test]
    fn late_cameras_while_suspended_wait_for_resume() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &[], Options::default());
        assert!(commands(&c.set_cameras(&spec(), &cameras(), now)).is_empty());
        let (_, cam, _) = start_of(&c.resume(now));
        assert_eq!(cam, "main");
    }

    #[test]
    fn while_recording_lenses_switch_but_flip_and_photos_wait() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        assert_eq!(c.recording_source().as_deref(), Some("main"));
        c.set_recording(true);
        assert!(!c.view().recording, "only in video mode");
        c.set_mode(Mode::Video, now);
        c.set_recording(true);
        assert!(c.view().recording);
        c.set_mode(Mode::Photo, now);
        assert_eq!(c.view().mode, Mode::Video, "no mode change mid-recording");
        // The zoom picks lenses as ever: the recorder follows the camera.
        let (g, cam, _) = start_of(&c.set_zoom(6.0, ZoomSource::Preset, now));
        assert_eq!(cam, "tele");
        c.on_event(started(g, "tele"), now);
        c.on_event(frame(g, "tele", 33_000), now);
        let (_, cam, _) = start_of(&c.set_zoom(0.5, ZoomSource::Preset, now));
        assert_eq!(cam, "uw");
        assert!(c.flip(now).is_empty());
        assert!(c.shutter(now).is_empty());
        c.set_recording(false);
    }

    /// A frame whose exposure says the scene is too dim for the tele: on
    /// the main at 1/30 s and gain 6, the tele would need gain 21.
    fn dim_frame(generation: u64, camera: &str) -> Event {
        let mut e = frame(generation, camera, 33_333);
        if let Event::Frame(f) = &mut e {
            f.meta.analog_gain = Some(6.0);
        }
        e
    }

    #[test]
    fn in_low_light_the_main_camera_stands_in_for_the_tele() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let g = open(&mut c, t0);
        // Dim, on the main at 1x: judged, but nothing to switch.
        for n in (0..=1600).step_by(100) {
            assert!(commands(&c.on_event(dim_frame(g, "main"), ms(n))).is_empty());
        }
        assert!(!c.view().low_light);
        // 5x in the dark: the main crops 5x instead of starting the tele.
        let out = c.set_zoom(5.0, ZoomSource::Preset, ms(1700));
        assert!(commands(&out).is_empty(), "{out:?}");
        let v = c.view();
        assert!(v.low_light);
        assert_eq!(v.lens_role, Some(Role::Main));
        assert_eq!(c.transform("main").unwrap().scale, 5.0);
        // Bright again for three seconds: back to the tele.
        let mut started = None;
        for n in (1800..=5000).step_by(100) {
            let out = c.on_event(frame(g, "main", 5_000), ms(n));
            if let [Command::Start { camera, .. }] = commands(&out).as_slice() {
                started = Some((n, camera.clone()));
                break;
            }
        }
        let (n, cam) = started.expect("switched back");
        assert_eq!(cam, "tele");
        assert!(n >= 4800, "after the dwell, at {n} ms");
        assert!(!c.view().low_light);
    }

    #[test]
    fn low_light_on_the_tele_falls_back_and_pro_or_the_setting_prevent_it() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, t0);
        let (g, cam, _) = start_of(&c.set_zoom(5.0, ZoomSource::Preset, t0));
        assert_eq!(cam, "tele");
        c.on_event(started(g, "tele"), t0);
        // The tele at 1/30 s and gain 12.
        let tele_dim = |n| {
            let mut e = frame(g, "tele", 33_333);
            if let Event::Frame(f) = &mut e {
                f.meta.analog_gain = Some(12.0);
                f.meta.sequence = n;
            }
            e
        };
        let mut switched = None;
        for n in 0..30u64 {
            let out = c.on_event(tele_dim(n as u32), ms(n * 100));
            if let [Command::Start {
                camera, generation, ..
            }] = commands(&out).as_slice()
            {
                switched = Some((n * 100, camera.clone(), *generation));
                break;
            }
        }
        let (at, cam, g) = switched.expect("fell back");
        assert_eq!(cam, "main");
        assert!(at >= 1500, "{at}");
        c.on_event(started(g, "main"), ms(3500));
        let settle = |c: &mut Controller, out: Vec<Output>| {
            let (g, cam, _) = start_of(&out);
            c.on_event(started(g, &cam), ms(4000));
            cam
        };
        // The Pro panel puts the tele back, and keeps it while open.
        let out = c.set_pro(true, ms(4000));
        assert_eq!(settle(&mut c, out), "tele");
        let out = c.set_pro(false, ms(4100));
        assert_eq!(settle(&mut c, out), "main");
        // So does turning the setting off.
        let out = c.set_low_light(false, ms(4200));
        assert_eq!(settle(&mut c, out), "tele");
    }

    #[test]
    fn the_ultrawide_has_no_low_light_fallback() {
        let t0 = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let g = open(&mut c, t0);
        for n in 0..30u64 {
            c.on_event(dim_frame(g, "main"), t0 + Duration::from_millis(n * 100));
        }
        let (_, cam, _) = start_of(&c.set_zoom(0.5, ZoomSource::Preset, t0));
        assert_eq!(cam, "uw");
        assert!(!c.view().low_light);
    }

    #[test]
    fn sixty_fps_only_where_the_mode_allows_and_only_for_video() {
        let now = Instant::now();
        let mut c = Controller::new(
            &spec(),
            &cameras(),
            Options {
                video_fps: 60,
                ..Options::default()
            },
        );
        let (g, _, _) = start_of(&c.resume(now));
        // No frame durations reported: nothing asked, 60 not offered.
        c.on_event(started(g, "main"), now);
        c.on_event(frame(g, "main", 10_000), now);
        assert!(!c.view().can_60fps);
        assert!(commands(&c.set_mode(Mode::Video, now)).is_empty());
        c.set_mode(Mode::Photo, now);
        // A mode that reaches 60 fps.
        let (g, _, _) = start_of(&c.set_zoom(0.5, ZoomSource::Preset, now));
        let mut e = started(g, "uw");
        if let Event::Started {
            max_fps, min_fps, ..
        } = &mut e
        {
            *max_fps = Some(60.0);
            *min_fps = Some(5.0);
        }
        c.on_event(e, now);
        c.on_event(frame(g, "uw", 10_000), now);
        assert!(c.view().can_60fps);
        let duration = |out: &[Output]| match commands(out).as_slice() {
            [Command::SetControls { controls, .. }] => controls.frame_duration_us,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            duration(&c.set_mode(Mode::Video, now)),
            Some((16_667, 16_667))
        );
        assert_eq!(duration(&c.set_video_fps(30)), Some((33_333, 33_333)));
        assert_eq!(duration(&c.set_video_fps(60)), Some((16_667, 16_667)));
        // Back to photos: the 60 fps limit lifts, and dim scenes may
        // lengthen frames to the mode's longest (5 fps here).
        assert_eq!(
            duration(&c.set_mode(Mode::Photo, now)),
            Some((33_333, 200_000))
        );
    }

    #[test]
    fn photos_let_the_exposure_lengthen_frames() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let reported = |g: u64, camera: &str| {
            let mut e = started(g, camera);
            if let Event::Started {
                max_fps, min_fps, ..
            } = &mut e
            {
                *max_fps = Some(30.0);
                *min_fps = Some(7.5);
            }
            e
        };
        // What the last command asks of the frame durations.
        let durations = |out: &[Output]| {
            commands(out)
                .iter()
                .rev()
                .find_map(|cmd| match cmd {
                    Command::Start { controls, .. } | Command::SetControls { controls, .. } => {
                        Some(controls.frame_duration_us)
                    }
                    _ => None,
                })
                .flatten()
        };
        let (g, _, _) = start_of(&c.resume(now));
        c.on_event(reported(g, "main"), now);
        c.on_event(frame(g, "main", 10_000), now);
        // Away and back: the start asks for 30 fps at most, frames up to
        // the mode's longest (7.5 fps).
        let (g, _, _) = start_of(&c.set_zoom(0.5, ZoomSource::Preset, now));
        c.on_event(started(g, "uw"), now);
        c.on_event(frame(g, "uw", 10_000), now);
        let out = c.set_zoom(1.0, ZoomSource::Preset, now);
        let (g, cam, _) = start_of(&out);
        assert_eq!(cam, "main");
        assert_eq!(durations(&out), Some((33_333, 133_333)));
        c.on_event(reported(g, "main"), now);
        c.on_event(frame(g, "main", 10_000), now);
        // Video holds its frame rate, photos lengthen again.
        assert_eq!(
            durations(&c.set_mode(Mode::Video, now)),
            Some((33_333, 33_333))
        );
        assert_eq!(
            durations(&c.set_mode(Mode::Photo, now)),
            Some((33_333, 133_333))
        );
    }

    #[test]
    fn a_video_tap_moves_continuous_af_to_the_window() {
        let now = Instant::now();
        let mut cams = cameras();
        for c in cams.iter_mut() {
            c.af_windows = true;
            c.crop_maximum = Some(crate::backend::Rect {
                x: 0,
                y: 0,
                width: 4080,
                height: 3072,
            });
        }
        let mut c = Controller::new(&spec(), &cams, Options::default());
        open(&mut c, now);
        c.set_mode(Mode::Video, now);
        match commands(&c.tap(0.5, 0.5, None, now)).as_slice() {
            [Command::SetControls { controls, .. }] => {
                assert!(controls.af_continuous);
                assert!(controls.af_window.is_some());
                assert!(controls.focus.is_none(), "no one-shot scan");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            c.view().focus.map(|f| f.status),
            Some(focus::Status::Following)
        );
    }

    #[test]
    fn every_lens_frames_the_view_alike() {
        // With axes off center, at each switch the old and the new lens
        // look at the same point of the scene.
        let spec = TableSpec::parse(
            r#"
max_zoom = 15.0
[[lens]]
role = "main"
id = "main"
zoom = 1.0
[[lens]]
role = "ultrawide"
id = "uw"
zoom = 0.45
axis = [0.47, 0.5]
[[lens]]
role = "tele"
id = "tele"
zoom = 4.78
axis = [0.56, 0.48]
"#,
        )
        .unwrap();
        let now = Instant::now();
        let mut c = Controller::new(&spec, &cameras(), Options::default());
        // Where the view's center is in the main camera's picture, seen
        // from each lens's frame.
        let in_main = |c: &Controller, cam: &str, zoom: f64, axis: (f64, f64)| {
            let t = c.transform(cam).unwrap();
            (
                axis.0 + (t.center.0 - 0.5) / zoom,
                axis.1 + (t.center.1 - 0.5) / zoom,
            )
        };
        for z in [0.99, 1.0, 4.77, 4.78] {
            c.set_zoom(z, ZoomSource::Preset, now);
            let a = in_main(&c, "main", 1.0, (0.5, 0.5));
            let b = if z < 1.0 {
                in_main(&c, "uw", 0.45, (0.47, 0.5))
            } else {
                in_main(&c, "tele", 4.78, (0.56, 0.48))
            };
            if (1.0..4.78).contains(&z) {
                continue; // the tele can't show below its own factor
            }
            assert!(
                (a.0 - b.0).abs() < 2e-3 && (a.1 - b.1).abs() < 2e-3,
                "{z}: {a:?} {b:?}"
            );
        }
    }

    fn caps_all() -> crate::backend::Caps {
        crate::backend::Caps {
            ev: Some((-8.0, 8.0)),
            exposure_time: Some((100, 66_000)),
            gain: Some((1.0, 16.0)),
            manual_exposure: true,
            awb_modes: true,
            color_temperature: Some((2500, 7500)),
            lens_position: Some((0.0, 1.0)),
            ..Default::default()
        }
    }

    fn settings_of(out: &[Output]) -> crate::backend::ExposureSettings {
        match commands(out).as_slice() {
            [Command::SetControls { controls, .. }] => controls.settings.expect("settings"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_tap_meters_and_the_slider_adds_and_a_lens_switch_resets() {
        let now = Instant::now();
        let mut cams = cameras();
        for c in cams.iter_mut() {
            c.caps = caps_all();
        }
        let mut c = Controller::new(&spec(), &cams, Options::default());
        open(&mut c, now);
        // A dark area: +2 EV.
        let s = settings_of(&c.tap(0.5, 0.5, Some(29.5), now));
        assert!((s.ev - 2.0).abs() < 1e-3);
        let s = settings_of(&c.set_ev(-1.0));
        assert!((s.ev - 1.0).abs() < 1e-3);
        assert_eq!(c.view().exposure.ev_user, -1.0);
        // Switching to the tele: a fresh start, EV back to 0.
        match commands(&c.set_zoom(5.0, ZoomSource::Preset, now)).as_slice() {
            [Command::Start { controls, .. }] => {
                assert_eq!(controls.settings.unwrap().ev, 0.0);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(c.view().exposure.ev_user, 0.0);
    }

    #[test]
    fn lock_holds_what_the_camera_reported_and_a_tap_releases_it() {
        let now = Instant::now();
        let mut cams = cameras();
        for c in cams.iter_mut() {
            c.caps = caps_all();
        }
        let mut c = Controller::new(&spec(), &cams, Options::default());
        open(&mut c, now); // its frame reported 10 ms at gain 1
        let s = settings_of(&c.lock(now));
        assert_eq!(s.shutter, Setting::Manual(10_000));
        assert_eq!(s.gain, Setting::Manual(1.0));
        assert!(c.view().exposure.locked);
        let s = settings_of(&c.tap(0.5, 0.5, None, now));
        assert_eq!(s.shutter, Setting::Auto);
        assert!(!c.view().exposure.locked);
    }

    #[test]
    fn without_the_controls_lock_explains_and_pro_values_stay_auto() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        assert!(matches!(c.lock(now).as_slice(), [Output::Error(_)]));
        let s = settings_of(&c.set_shutter(Setting::Manual(4_000)));
        assert_eq!(
            s.shutter,
            Setting::Auto,
            "libcamera 4.8 offers no manual exposure"
        );
        assert_eq!(
            c.view().exposure.pro.shutter,
            Setting::Manual(4_000),
            "kept for when it does"
        );
    }

    #[test]
    fn pro_values_reach_stills_and_keep_across_lenses() {
        let now = Instant::now();
        let mut cams = cameras();
        for c in cams.iter_mut() {
            c.caps = caps_all();
        }
        let mut c = Controller::new(&spec(), &cams, Options::default());
        open(&mut c, now);
        c.set_shutter(Setting::Manual(4_000));
        let s = settings_of(&c.set_iso(Setting::Manual(400)));
        assert_eq!(s.gain, Setting::Manual(8.0), "ISO 400 at base 50");
        let s = settings_of(&c.set_lens_position(Setting::Manual(0.3)));
        assert_eq!(s.lens, Setting::Manual(0.3));
        match commands(&c.shutter(now)).as_slice() {
            [Command::Start {
                stream, controls, ..
            }] => {
                assert_eq!(stream.purpose, Purpose::Still);
                let s = controls.settings.unwrap();
                assert_eq!(s.shutter, Setting::Manual(4_000));
                assert_eq!(s.lens, Setting::Manual(0.3));
                assert!(!controls.af_continuous);
            }
            other => panic!("{other:?}"),
        }
        let mut c = Controller::new(&spec(), &cams, Options::default());
        open(&mut c, now);
        c.set_shutter(Setting::Manual(4_000));
        match commands(&c.set_zoom(0.5, ZoomSource::Preset, now)).as_slice() {
            [Command::Start { controls, .. }] => {
                assert_eq!(controls.settings.unwrap().shutter, Setting::Manual(4_000));
            }
            other => panic!("{other:?}"),
        }
        let s = settings_of(&{
            let g = 2;
            c.on_event(started(g, "uw"), now);
            c.on_event(frame(g, "uw", 4_000), now);
            c.reset_exposure()
        });
        assert_eq!(s, crate::backend::ExposureSettings::default());
    }

    /// The main, ultrawide and front cameras and a macro lens.
    fn macro_controller() -> Controller {
        let cam = |id: &str, location| CameraInfo {
            id: id.into(),
            location,
            rotation: 90,
            model: "sensor".into(),
            autofocus: id == "main",
            ..CameraInfo::default()
        };
        let cams = vec![
            cam("macro", Location::Back),
            cam("front", Location::Front),
            cam("main", Location::Back),
            cam("uw", Location::Back),
        ];
        let spec = TableSpec::parse(
            r#"
[[lens]]
role = "main"
id = "main"
zoom = 1.0
autofocus = true
[[lens]]
role = "ultrawide"
id = "uw"
zoom = 0.45
[[lens]]
role = "front"
id = "front"
zoom = 1.0
[[lens]]
role = "macro"
id = "macro"
zoom = 1.0
preview = [1296, 970]
still = [2592, 1940]
"#,
        )
        .unwrap();
        Controller::new(&spec, &cams, Options::default())
    }

    #[test]
    fn the_lens_row_has_a_button_for_each_lens_and_marks_the_chosen_one() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        let row = |c: &Controller| {
            c.view()
                .buttons
                .iter()
                .map(|b| (b.zoom, b.role, b.native, b.active))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            row(&c),
            vec![
                (0.45, Role::Ultrawide, true, false),
                (1.0, Role::Main, true, true),
                (2.0, Role::Main, false, false),
                (5.0, Role::Tele, true, false),
            ]
        );
        c.set_zoom(3.0, ZoomSource::Preset, now);
        assert!(row(&c)[2].3, "the 2 stage shows 3x");
    }

    #[test]
    fn a_tapped_point_holds_until_given_up_and_a_fresh_start_forgets_it() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        c.tap(0.3, 0.4, Some(60.0), now);
        assert_eq!(c.view().tap_point, Some((0.3, 0.4)));
        // The ring goes after the hold; the point still counts.
        c.tick(now + Duration::from_secs(3));
        c.tick(now + Duration::from_secs(10));
        assert!(c.view().focus.is_none());
        assert_eq!(c.view().tap_point, Some((0.3, 0.4)));
        // Given up: the whole picture again, said to the camera.
        match commands(&c.clear_tap()).as_slice() {
            [Command::SetControls { controls, .. }] => {
                assert!(controls.whole_frame_af && controls.af_continuous);
                assert!(!controls.settings.unwrap().spot);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(c.view().tap_point, None);
        // Back from the background: forgotten too.
        c.tap(0.6, 0.6, None, now);
        c.suspend(now);
        c.resume(now);
        assert_eq!(c.view().tap_point, None);
        assert_eq!(c.view().exposure.ev_user, 0.0);
    }

    #[test]
    fn a_held_lens_never_switches_while_zooming() {
        let now = Instant::now();
        let opts = Options {
            hold_lens: true,
            ..Options::default()
        };
        let mut c = Controller::new(&spec(), &cameras(), opts);
        open(&mut c, now);
        // A pinch to 8x stays on the main camera, settled or released.
        assert!(commands(&c.set_zoom(8.0, ZoomSource::Gesture, now)).is_empty());
        assert!(commands(&c.tick(now + Duration::from_secs(1))).is_empty());
        assert!(commands(&c.zoom_released(now + Duration::from_secs(1))).is_empty());
        assert_eq!(c.view().zoom, 8.0);
        assert_eq!(c.view().lens_role, Some(Role::Main));
        // Nor below its own view.
        c.set_zoom(0.5, ZoomSource::Gesture, now);
        assert_eq!((c.view().zoom, c.view().zoom_floor), (1.0, 1.0));
        // The ultrawide's button chooses it; zooming then crops it.
        let (_, cam, _) = start_of(&c.press_zoom_button(0.45, now));
        assert_eq!(cam, "uw");
        assert!(commands(&c.set_zoom(2.5, ZoomSource::Gesture, now)).is_empty());
        let v = c.view();
        assert_eq!((v.lens_role, v.zoom_floor), (Some(Role::Ultrawide), 0.45));
        assert!(v.buttons[0].active, "the .5 button shows 2.5x");
        assert!((c.transform("uw").unwrap().scale - 2.5 / 0.45).abs() < 1e-9);
    }

    #[test]
    fn a_held_lens_has_no_low_light_stand_in() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        c.press_zoom_button(5.0, now);
        c.back.as_mut().unwrap().fallback = true;
        assert!(c.view().low_light);
        c.set_hold_lens(true, now);
        assert!(!c.view().low_light);
        assert_eq!(c.view().lens_role, Some(Role::Tele));
    }

    #[test]
    fn a_lens_button_leaves_the_macro_lens() {
        let now = Instant::now();
        let mut c = macro_controller();
        open(&mut c, now);
        let (g, _, _) = start_of(&c.toggle_macro(now));
        c.on_event(started(g, "macro"), now);
        assert!(c.view().macro_on);
        assert!(c.view().buttons.iter().all(|b| !b.active));
        let (_, cam, _) = start_of(&c.press_zoom_button(1.0, now));
        assert_eq!(cam, "main");
        assert!(!c.view().macro_on);
    }

    #[test]
    fn the_macro_lens_is_turned_on_and_off_and_zooms_on_its_own() {
        let now = Instant::now();
        let mut c = macro_controller();
        open(&mut c, now);
        let v = c.view();
        assert!(v.can_macro && !v.macro_on);
        // Not on the zoom ladder: the widest zoom is the ultrawide's.
        let (g, cam, _) = start_of(&c.set_zoom(0.45, ZoomSource::Preset, now));
        assert_eq!(cam, "uw");
        c.on_event(started(g, "uw"), now);
        c.on_event(frame(g, "uw", 10_000), now);
        let (g, cam, _) = start_of(&c.toggle_macro(now));
        assert_eq!(cam, "macro");
        c.on_event(started(g, "macro"), now);
        c.on_event(frame(g, "macro", 10_000), now);
        let v = c.view();
        assert!(v.macro_on);
        assert_eq!(v.presets, MACRO_PRESETS.to_vec());
        assert_eq!(v.lens_role, Some(Role::Macro));
        // Its zoom is a crop of its own frames, up to 4x.
        assert!(commands(&c.set_zoom(8.0, ZoomSource::Preset, now)).is_empty());
        assert_eq!(c.view().zoom, crate::lens::MACRO_MAX_ZOOM);
        assert_eq!(c.transform("macro").unwrap().scale, 4.0);
        // The ladder's lenses show their whole view meanwhile.
        assert_eq!(c.transform("uw").unwrap().scale, 1.0);
        // Off: back to the ladder at its zoom.
        let (_, cam, _) = start_of(&c.toggle_macro(now));
        assert_eq!(cam, "uw");
        assert!(!c.view().macro_on);
    }

    #[test]
    fn a_flip_leaves_the_macro_lens_and_the_front_has_none() {
        let now = Instant::now();
        let mut c = macro_controller();
        open(&mut c, now);
        let (g, cam, _) = start_of(&c.toggle_macro(now));
        assert_eq!(cam, "macro");
        c.on_event(started(g, "macro"), now);
        let (g, cam, _) = start_of(&c.flip(now));
        assert_eq!(cam, "front");
        c.on_event(started(g, "front"), now);
        let v = c.view();
        assert!(!v.can_macro && !v.macro_on);
        assert!(c.toggle_macro(now).is_empty());
        let (_, cam, _) = start_of(&c.flip(now));
        assert_eq!(cam, "main");
    }

    #[test]
    fn without_a_macro_lens_there_is_nothing_to_turn_on() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        assert!(!c.view().can_macro);
        assert!(c.toggle_macro(now).is_empty());
    }

    #[test]
    fn flip_goes_to_the_front_camera_and_back() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        let (g, cam, _) = start_of(&c.flip(now));
        assert_eq!(cam, "front");
        assert_eq!(c.view().presets, vec![1.0, 2.0]);
        c.on_event(started(g, "front"), now);
        c.on_event(frame(g, "front", 5_000), now);
        let t = c.transform("front").unwrap();
        assert!(t.mirror);
        let (_, cam, _) = start_of(&c.flip(now));
        assert_eq!(cam, "main");
    }

    #[test]
    fn full_still_capture_reconfigures_and_comes_back() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        open(&mut c, now);
        let out = c.shutter(now);
        let (gs, cam, purpose) = start_of(&out);
        assert_eq!((cam.as_str(), purpose), ("main", Purpose::Still));
        match commands(&out)[0] {
            Command::Start { stream, .. } => {
                assert_eq!(stream.size, Some(Size::new(4080, 3072)));
                assert_eq!(stream.buffers, 2);
                assert_eq!(stream.fallbacks, vec![Size::new(2016, 1136)]);
            }
            _ => unreachable!(),
        }
        assert!(c.view().capturing);
        c.on_event(started(gs, "main"), now);
        // No AeState in these frames: a fixed wait (frames and time; the
        // full mode at 5 fps), then one is kept.
        let at = |i: u32| now + Duration::from_millis(200 * i as u64);
        for i in 0..capture::FIXED_DISCARD {
            assert!(c.on_event(frame(gs, "main", 10_000), at(i)).is_empty());
        }
        let out = c.on_event(frame(gs, "main", 10_000), at(capture::FIXED_DISCARD));
        assert!(matches!(out[0], Output::Picture(_)));
        let (gp, _, purpose) = start_of(&out[1..]);
        assert_eq!(purpose, Purpose::Preview);
        c.on_event(started(gp, "main"), now);
        let out = c.on_event(frame(gp, "main", 10_000), now);
        assert!(matches!(out[0], Output::Show(_)));
        assert!(!c.view().capturing);
    }

    #[test]
    fn a_table_without_bursts_takes_single_photos_in_night_and_hdr() {
        let now = Instant::now();
        let no_bursts = TableSpec::parse(&format!("bursts = false\n{SPEC}")).unwrap();
        for (bursts, spec) in [(true, spec()), (false, no_bursts)] {
            for burst in [capture::BurstSpec::NIGHT, capture::BurstSpec::HDR] {
                let opts = Options {
                    capture_mode: capture::Mode::Burst(burst),
                    ..Options::default()
                };
                let mut c = Controller::new(&spec, &cameras(), opts);
                let (g, _, _) = start_of(&c.resume(now));
                let mut e = started(g, "main");
                if let Event::Started { still, .. } = &mut e {
                    *still = Some(Size::new(4080, 3072));
                }
                c.on_event(e, now);
                c.on_event(frame(g, "main", 10_000), now);
                let out = c.shutter(now);
                if bursts {
                    assert_eq!(c.capture.burst(), Some(burst));
                } else {
                    // A photo as in photo mode: from the still stream
                    // beside the preview.
                    assert_eq!(c.capture.burst(), None);
                    assert!(
                        matches!(commands(&out).as_slice(), [Command::Capture { generation }] if *generation == g),
                        "{out:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_still_stream_beside_the_preview_takes_photos_without_a_restart() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let out = c.resume(now);
        let g = match commands(&out).as_slice() {
            [Command::Start {
                generation, stream, ..
            }] => {
                assert_eq!(stream.still, Some(Size::new(4080, 3072)));
                assert!(!stream.video);
                *generation
            }
            other => panic!("{other:?}"),
        };
        let mut e = started(g, "main");
        if let Event::Started { still, .. } = &mut e {
            *still = Some(Size::new(4080, 3072));
        }
        c.on_event(e, now);
        c.on_event(frame(g, "main", 10_000), now);
        let out = c.shutter(now);
        assert!(
            matches!(commands(&out).as_slice(), [Command::Capture { generation }] if *generation == g),
            "{out:?}"
        );
        // Preview frames keep showing; the still frame is the photo.
        let out = c.on_event(frame(g, "main", 10_000), now);
        assert!(matches!(out.as_slice(), [Output::Show(_)]));
        let mut still = frame(g, "main", 10_000);
        if let Event::Frame(f) = &mut still {
            f.still = true;
        }
        let out = c.on_event(still, now + Duration::from_millis(150));
        match out.as_slice() {
            [Output::Picture(p)] => assert!(p.frame.still),
            other => panic!("{other:?}"),
        }
        assert!(!c.view().capturing);
    }

    fn lights(out: &[Output]) -> Vec<Light> {
        out.iter()
            .filter_map(|o| match o {
                Output::Light(l) => Some(*l),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_flash_photo_beside_the_preview_waits_under_the_light() {
        let now = Instant::now();
        let at = |ms: u64| now + Duration::from_millis(ms);
        let opts = Options {
            flash: true,
            flash_mode: FlashMode::On,
            ..Options::default()
        };
        let mut c = Controller::new(&spec(), &cameras(), opts);
        let (g, _, _) = start_of(&c.resume(now));
        let mut e = started(g, "main");
        if let Event::Started { still, .. } = &mut e {
            *still = Some(Size::new(4080, 3072));
        }
        assert!(lights(&c.on_event(e, now)).is_empty(), "no torch");
        c.on_event(frame(g, "main", 66_000), now);
        // The shutter lights the LED and asks for nothing yet.
        let out = c.shutter(now);
        assert_eq!(lights(&out), vec![Light::Photo]);
        assert!(commands(&out).is_empty(), "{out:?}");
        assert!(c.view().capturing);
        assert_eq!(c.view().light, Light::Photo);
        // A second press while waiting does nothing; nor do zoom or flip.
        assert!(c.shutter(now).is_empty());
        assert!(c.flip(now).is_empty());
        // Frames under the light: no AeState here, so the fixed wait.
        let mut asked = None;
        for i in 1..=crate::flash::PRE_FLASH_FIXED {
            let out = c.on_event(frame(g, "main", 20_000), at(50 * i as u64));
            if let [Command::Capture { generation }] = commands(&out).as_slice() {
                asked = Some((i, *generation));
                break;
            }
        }
        assert_eq!(asked, Some((crate::flash::PRE_FLASH_FIXED, g)));
        assert_eq!(c.view().light, Light::Photo, "lit until the still comes");
        let mut still = frame(g, "main", 20_000);
        if let Event::Frame(f) = &mut still {
            f.still = true;
        }
        let out = c.on_event(still, at(2000));
        assert!(matches!(out.first(), Some(Output::Picture(_))), "{out:?}");
        assert_eq!(lights(&out), vec![Light::Off]);
        assert!(!c.view().capturing);
    }

    #[test]
    fn a_photo_right_after_the_torch_waits_for_the_exposure() {
        let now = Instant::now();
        let at = |ms: u64| now + Duration::from_millis(ms);
        let opts = Options {
            flash: true,
            flash_mode: FlashMode::Torch,
            ..Options::default()
        };
        let mut c = Controller::new(&spec(), &cameras(), opts);
        let (g, _, _) = start_of(&c.resume(now));
        let mut e = started(g, "main");
        if let Event::Started { still, .. } = &mut e {
            *still = Some(Size::new(4080, 3072));
        }
        assert_eq!(lights(&c.on_event(e, now)), vec![Light::Torch]);
        // Under the torch the exposure is short.
        c.on_event(frame(g, "main", 10_000), now);
        assert_eq!(lights(&c.set_flash_mode(FlashMode::Off)), vec![Light::Off]);
        // The shutter at once: nothing asked yet, the photo waits.
        let out = c.shutter(at(10));
        assert!(commands(&out).is_empty(), "{out:?}");
        assert!(c.view().capturing);
        assert!(c.shutter(at(20)).is_empty(), "one photo");
        // The exposure walks back up in the dark, then holds.
        let mut asked = None;
        let walk = [10_000, 25_000].into_iter().chain([40_000; 12]);
        for (i, exposure) in walk.enumerate() {
            let ms = 50 * (i as u64 + 1);
            let out = c.on_event(frame(g, "main", exposure), at(ms));
            if let [Command::Capture { .. }] = commands(&out).as_slice() {
                asked = Some(ms);
                break;
            }
        }
        let ms = asked.expect("the photo once the exposure settled");
        assert!(ms >= crate::flash::PRE_FLASH_MIN.as_millis() as u64, "{ms}");

        // Once the exposure has settled, a photo goes ahead at once.
        let mut c = Controller::new(
            &spec(),
            &cameras(),
            Options {
                flash: true,
                flash_mode: FlashMode::Torch,
                ..Options::default()
            },
        );
        let (g, _, _) = start_of(&c.resume(now));
        let mut e = started(g, "main");
        if let Event::Started { still, .. } = &mut e {
            *still = Some(Size::new(4080, 3072));
        }
        c.on_event(e, now);
        c.on_event(frame(g, "main", 10_000), now);
        c.set_flash_mode(FlashMode::Off);
        for (i, exposure) in [30_000].into_iter().chain([40_000; 12]).enumerate() {
            c.on_event(frame(g, "main", exposure), at(50 * (i as u64 + 1)));
        }
        let out = c.shutter(at(700));
        assert!(
            matches!(commands(&out).as_slice(), [Command::Capture { .. }]),
            "{out:?}"
        );
    }

    #[test]
    fn a_flash_photo_goes_ahead_without_frames_and_stops_with_the_app() {
        let now = Instant::now();
        let opts = Options {
            flash: true,
            flash_mode: FlashMode::On,
            ..Options::default()
        };
        let mut c = Controller::new(&spec(), &cameras(), opts.clone());
        let g = open(&mut c, now);
        assert_eq!(lights(&c.shutter(now)), vec![Light::Photo]);
        // No frame comes: the photo is taken anyway at the longest wait
        // (the main camera's still mode, here, with a stop and a start).
        assert!(commands(&c.tick(now + Duration::from_millis(500))).is_empty());
        let out = c.tick(now + crate::flash::PRE_FLASH_MAX);
        assert!(
            matches!(commands(&out).as_slice(), [Command::Start { generation, stream, .. }]
                if *generation > g && stream.purpose == Purpose::Still),
            "{out:?}"
        );
        assert!(lights(&out).is_empty(), "still lit for the still");
        // Put away mid-photo: the light goes out.
        let out = c.suspend(now + Duration::from_secs(3));
        assert_eq!(lights(&out), vec![Light::Off]);
        // Off, or no LED: nothing lights.
        for (flash, mode) in [
            (true, FlashMode::Off),
            (false, FlashMode::On),
            (false, FlashMode::Torch),
        ] {
            let mut c = Controller::new(
                &spec(),
                &cameras(),
                Options {
                    flash,
                    flash_mode: mode,
                    ..Options::default()
                },
            );
            open(&mut c, now);
            assert!(lights(&c.shutter(now)).is_empty(), "{flash} {mode:?}");
        }
    }

    #[test]
    fn the_torch_follows_the_mode_and_the_side() {
        let now = Instant::now();
        let opts = Options {
            flash: true,
            ..Options::default()
        };
        let mut c = Controller::new(&spec(), &cameras(), opts);
        let g = open(&mut c, now);
        assert_eq!(
            lights(&c.set_flash_mode(FlashMode::Torch)),
            vec![Light::Torch]
        );
        // Zooming to another back lens keeps it lit.
        let out = c.set_zoom(5.0, ZoomSource::Preset, now);
        assert!(lights(&out).is_empty());
        let (g2, _, _) = start_of(&out);
        assert!(g2 > g);
        assert!(lights(&c.on_event(Event::Stopped { generation: g }, now)).is_empty());
        assert!(lights(&c.on_event(started(g2, "tele"), now)).is_empty());
        // The front side has no flash.
        let out = c.flip(now);
        assert_eq!(lights(&out), vec![Light::Off]);
        assert_eq!(c.view().flash_mode, FlashMode::Torch, "kept for the back");
        assert!(!c.view().can_flash);
        // A camera that fails puts it out.
        let mut c = Controller::new(
            &spec(),
            &cameras(),
            Options {
                flash: true,
                flash_mode: FlashMode::Torch,
                ..Options::default()
            },
        );
        let (g, _, _) = start_of(&c.resume(now));
        assert!(lights(&c.resume(now)).is_empty(), "not before it streams");
        assert_eq!(
            lights(&c.on_event(started(g, "main"), now)),
            vec![Light::Torch]
        );
        let out = c.on_event(
            Event::Error {
                generation: Some(g),
                message: "gone".into(),
            },
            now,
        );
        assert_eq!(lights(&out), vec![Light::Off]);
        assert_eq!(lights(&c.set_flash_mode(FlashMode::Off)), vec![]);
    }

    #[test]
    fn a_camera_without_the_still_stream_is_not_asked_again_or_restarted() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let g = open(&mut c, now);
        // `open` started it with no still stream: the next photo
        // reconfigures as before, and nothing restarts the camera for it.
        assert!(commands(&c.set_zoom(1.5, ZoomSource::Preset, now)).is_empty());
        assert!(commands(&c.set_mode(Mode::Video, now)).is_empty());
        assert!(commands(&c.set_mode(Mode::Photo, now)).is_empty());
        assert_eq!(
            start_of(&c.shutter(now)),
            (g + 1, "main".to_string(), Purpose::Still)
        );
    }

    #[test]
    fn video_mode_starts_the_lens_tables_video_stream() {
        let now = Instant::now();
        let spec = TableSpec::parse(
            r#"
[[lens]]
role = "main"
id = "main"
zoom = 1.0
preview = [2304, 1728]
still = [4624, 3472]
video = [1920, 1080]
"#,
        )
        .unwrap();
        let mut c = Controller::new(&spec, &cameras()[..1], Options::default());
        let g = open(&mut c, now);
        match commands(&c.set_mode(Mode::Video, now)).as_slice() {
            [Command::Start {
                generation, stream, ..
            }] => {
                assert_eq!(*generation, g + 1);
                assert!(stream.video);
                assert_eq!(stream.size, Some(Size::new(1920, 1080)));
                assert_eq!(stream.still, None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quick_capture_takes_the_next_preview_frame_with_the_zoom_crop() {
        let now = Instant::now();
        let opts = Options {
            capture_mode: capture::Mode::Quick,
            ..Options::default()
        };
        let mut c = Controller::new(&spec(), &cameras(), opts);
        let g = open(&mut c, now);
        c.set_zoom(2.0, ZoomSource::Preset, now);
        assert!(commands(&c.shutter(now)).is_empty());
        match c.on_event(frame(g, "main", 10_000), now).as_slice() {
            [Output::Picture(p)] => {
                assert_eq!(p.crop, 2.0);
                assert_eq!(p.rotation, 270, "main: Rotation 90 is 270 clockwise");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_busy_camera_is_retried() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let (g, _, _) = start_of(&c.resume(now));
        let out = c.on_event(
            Event::Error {
                generation: Some(g),
                message: "busy".into(),
            },
            now,
        );
        assert!(matches!(out.last(), Some(Output::Error(_))));
        assert!(commands(&c.tick(now + Duration::from_secs(1))).is_empty());
        let (g2, cam, _) = start_of(&c.tick(now + RETRY_AFTER));
        assert!(g2 > g);
        assert_eq!(cam, "main");
    }

    #[test]
    fn suspend_stops_and_releases_and_resume_restarts() {
        let now = Instant::now();
        let mut c = Controller::new(&spec(), &cameras(), Options::default());
        let g = open(&mut c, now);
        let out = c.suspend(now);
        assert_eq!(
            commands(&out),
            vec![&Command::Stop { generation: g }, &Command::Release]
        );
        // Zoom while suspended changes the zoom but starts nothing.
        assert!(commands(&c.set_zoom(5.0, ZoomSource::Preset, now)).is_empty());
        c.on_event(Event::Stopped { generation: g }, now);
        let (_, cam, _) = start_of(&c.resume(now));
        assert_eq!(cam, "tele");
    }

    #[test]
    fn a_phone_without_a_tele_zooms_the_main_digitally() {
        let now = Instant::now();
        let mut cams = cameras();
        cams.retain(|c| c.id != "tele");
        let mut c = Controller::new(&spec(), &cams, Options::default());
        open(&mut c, now);
        assert!(commands(&c.set_zoom(5.0, ZoomSource::Preset, now)).is_empty());
        assert_eq!(c.transform("main").unwrap().scale, 5.0);
    }
}
