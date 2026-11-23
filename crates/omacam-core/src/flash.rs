//! The flash LED: lit for a photo, or steady as a torch.
//!
//! Phones like the Moto G60s have one white LED beside the back cameras,
//! driven by the kernel's LED class. Its strobe (a flash pulse) isn't used:
//! on the G60s the device tree caps the strobe current at the torch
//! current (175 mA) and its timeout at 384 ms, while the camera merges a
//! series of frames for each photo over one to two seconds, and nothing
//! ties a strobe to a frame's exposure. So a photo with the flash is the
//! usual LED-flash sequence on the steady light: light on, let exposure,
//! white balance and focus settle under it, take the photo, light off.
//! The steady light also needs no rights beyond the user's session (the
//! app sets it through logind, as the shell's torch does).
//!
//! Four modes ([`FlashMode`]): off; auto, which lights the LED for a photo
//! when the preview's exposure says the scene is dim
//! ([`FlashSpec::auto_above`]); on, for every photo; and torch, a steady
//! light on any back lens (the macro camera as a magnifier) that stays on
//! through photos and goes out when the app is put away, on the front
//! camera, and when the camera fails.
//!
//! How warm the LED may get is the kernel's business (the LED as a
//! cooling device): it may dim the LED or put it out on its own, and the
//! app doesn't fight it.
//!
//! [`PreFlash`] is the wait under the light before the photo: the
//! controller feeds it the preview's frames and starts the capture when
//! it says the picture has settled. On a lens that focuses by itself it
//! asks for one focus scan once the exposure has seen the light
//! ([`PreFlashStep::Focus`]): in a dim scene the focus found before the
//! light (if any) came from a picture mostly of noise, and the camera's
//! continuous autofocus takes its time to notice the change of light.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::backend::{AeState, AfState, FrameMeta};

/// What the flash does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlashMode {
    /// Never lit.
    #[default]
    Off,
    /// Lit for a photo when the scene is dim.
    Auto,
    /// Lit for every photo.
    On,
    /// A steady light while a back camera streams.
    Torch,
}

impl FlashMode {
    pub const ALL: [FlashMode; 4] = [
        FlashMode::Off,
        FlashMode::Auto,
        FlashMode::On,
        FlashMode::Torch,
    ];

    /// The next mode for a button that steps through them.
    pub fn next(self) -> FlashMode {
        match self {
            FlashMode::Off => FlashMode::Auto,
            FlashMode::Auto => FlashMode::On,
            FlashMode::On => FlashMode::Torch,
            FlashMode::Torch => FlashMode::Off,
        }
    }

    /// The name used in settings, scripts and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            FlashMode::Off => "off",
            FlashMode::Auto => "auto",
            FlashMode::On => "on",
            FlashMode::Torch => "torch",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FlashMode::Off => "Flash off",
            FlashMode::Auto => "Flash auto",
            FlashMode::On => "Flash on",
            FlashMode::Torch => "Torch",
        }
    }
}

impl std::str::FromStr for FlashMode {
    type Err = String;

    fn from_str(s: &str) -> Result<FlashMode, String> {
        FlashMode::ALL
            .into_iter()
            .find(|m| m.name() == s)
            .ok_or_else(|| format!("flash: auto, on, off or torch, not {s:?}"))
    }
}

/// The EXIF Flash tag's value for a photo: whether the light was on for
/// it (`fired`: lit for the photo, or the torch), and the mode. A camera
/// without a flash (the front one) has none.
pub fn exif_flash(available: bool, mode: FlashMode, fired: bool) -> u16 {
    if !available {
        return 0x20;
    }
    match (mode, fired) {
        (FlashMode::On, true) => 0x09,
        (FlashMode::Auto, true) => 0x19,
        (FlashMode::Auto, false) => 0x18,
        (FlashMode::Off, false) => 0x10,
        (_, true) => 0x01,
        (_, false) => 0x00,
    }
}

/// What the LED should do now. The app turns it into a brightness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Light {
    #[default]
    Off,
    /// The torch's steady light.
    Torch,
    /// Lit for a photo.
    Photo,
}

/// Which LED is the flash, from the lens table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Led {
    /// Look for one: an LED whose name says flash or torch.
    #[default]
    Find,
    /// The LED of this name (`/sys/class/leds/NAME`).
    Named(String),
    /// The phone has no flash the app may use.
    None,
}

/// The flash's entry in the lens table (`[flash]`).
#[derive(Debug, Clone, PartialEq)]
pub struct FlashSpec {
    pub led: Led,
    /// The torch's brightness (the LED's own steps); its maximum when the
    /// table doesn't say.
    pub torch: Option<u32>,
    /// The brightness for a photo; the LED's maximum when the table
    /// doesn't say.
    pub photo: Option<u32>,
    /// Auto lights the LED for a photo when the preview's exposure time
    /// (microseconds) times its gain (analog and digital) is above this.
    pub auto_above: f64,
}

/// Auto's threshold when the table gives none: a full frame at 30 fps
/// (33.3 ms) at gain 8, where [`crate::lowlight`] calls the light dim too.
pub const DEFAULT_AUTO_ABOVE: f64 = 33_333.0 * 8.0;

impl Default for FlashSpec {
    fn default() -> Self {
        FlashSpec {
            led: Led::Find,
            torch: None,
            photo: None,
            auto_above: DEFAULT_AUTO_ABOVE,
        }
    }
}

impl FlashSpec {
    /// No flash.
    pub fn none() -> FlashSpec {
        FlashSpec {
            led: Led::None,
            ..FlashSpec::default()
        }
    }

    /// The exposure demand of a frame: exposure time times gain. None
    /// when the frame doesn't say.
    pub fn demand(meta: &FrameMeta) -> Option<f64> {
        let t = meta.exposure_us? as f64;
        let gain = meta.analog_gain.unwrap_or(1.0) as f64 * meta.digital_gain.unwrap_or(1.0) as f64;
        (t > 0.0 && gain > 0.0).then_some(t * gain)
    }

    /// Whether auto lights the LED for a photo, judging by the last
    /// preview frame. Without one (or a frame that doesn't say) it
    /// doesn't.
    pub fn auto_fires(&self, last: Option<&FrameMeta>) -> bool {
        last.and_then(FlashSpec::demand)
            .is_some_and(|d| d > self.auto_above)
    }
}

/// The fewest preview frames to see under the light before the photo: the
/// sensor applies new exposure values two frames late, and the light
/// itself comes on a moment after it is asked for.
pub const PRE_FLASH_FRAMES: u32 = 4;
/// The least time under the light before the photo.
pub const PRE_FLASH_MIN: Duration = Duration::from_millis(300);
/// The longest wait under the light: past it the photo is taken as it is.
pub const PRE_FLASH_MAX: Duration = Duration::from_millis(3000);
/// The longest wait for the focus scan under the light, from asking for
/// it. A contrast scan on the Moto G60s took 15-25 frames (0.5-0.8 s at
/// 30 fps).
pub const PRE_FLASH_FOCUS: Duration = Duration::from_millis(1500);
/// A focus result counts without the scan having been seen after this
/// long (a camera that answers at once, or frames lost).
pub const PRE_FLASH_FOCUS_SEEN: Duration = Duration::from_millis(300);
/// The longest wait once the exposure has seen the light. The Moto G60s's
/// exposure loop saw the light after 300-500 ms and then walked its gain
/// down by about a tenth every 100-150 ms, still Searching after 2 s; its
/// still stream sets an exposure of its own, and photos taken 0.6 s after
/// the reaction came out as well exposed as those after 1.6 s (mean luma
/// 91 and 77 of 255).
pub const PRE_FLASH_AFTER: Duration = Duration::from_millis(700);
/// The exposure has seen the light when the demand (exposure time times
/// gain) falls under this share of the one before the light, or AE says
/// Searching. Before that, a Converged means nothing: on the Moto G60s
/// AE still said Converged four frames (300 ms) after the light came on,
/// at the old exposure, and that photo came out white.
pub const PRE_FLASH_DROP: f64 = 0.8;
/// No reaction by then: the light doesn't reach the scene (far away, or a
/// bright scene), so nothing will change.
pub const PRE_FLASH_REACT: Duration = Duration::from_millis(1000);
/// How close one frame's demand must be to the one before to count as
/// steady.
pub const PRE_FLASH_STEADY: f64 = 0.10;
/// Steady frames in a row that settle the exposure: after AE says it has
/// converged, or without AeState. (Not while AE says Searching: the
/// exposure loop may run slower than the frames, so a few frames with the
/// same values mean nothing.)
pub const PRE_FLASH_STEADY_FRAMES: u32 = 2;
/// Without AeState: the fewest frames to wait before the photo, as the
/// still sequence does ([`crate::capture::FIXED_DISCARD`]).
pub const PRE_FLASH_FIXED: u32 = crate::capture::FIXED_DISCARD;
/// How little the color gains may change from one frame to the next for
/// the white balance to count as settled under the light.
pub const PRE_FLASH_GAINS_STEADY: f64 = crate::capture::GAINS_STEADY;

/// The wait under the light before a flash photo.
#[derive(Debug, Clone, PartialEq)]
pub struct PreFlash {
    lit: Instant,
    frames: u32,
    last_gains: Option<[f32; 2]>,
    /// The demand of the last frame before the light.
    before: Option<f64>,
    last_demand: Option<f64>,
    /// When the exposure had seen the light.
    reacted: Option<Instant>,
    /// Frames in a row whose demand stayed within [`PRE_FLASH_STEADY`].
    steady: u32,
    /// The light went out instead of on: the demand rises.
    darker: bool,
    /// The focus scan under the light, on a lens that focuses.
    focus: Option<FocusWait>,
}

/// Where the focus scan under the light is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FocusWait {
    /// To be asked for once the exposure has seen the light.
    Due,
    /// Asked for at `at`; `scanning` once a frame said so.
    Asked { at: Instant, scanning: bool },
    /// Focused or failed.
    Done,
    /// The scan took too long.
    GaveUp,
}

/// How the wait under the light ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreFlashStep {
    /// Keep waiting.
    Wait,
    /// Settled: take the photo.
    Settled,
    /// The wait ran out: take the photo anyway.
    TimedOut,
    /// Ask the camera for one focus scan now (once); keep waiting.
    Focus,
}

impl PreFlash {
    /// The light was asked for at `now`; `before` is the last frame before
    /// it.
    pub fn new(now: Instant, before: Option<&FrameMeta>) -> PreFlash {
        let before = before.and_then(FlashSpec::demand);
        PreFlash {
            lit: now,
            frames: 0,
            last_gains: None,
            before,
            last_demand: before,
            reacted: None,
            steady: 0,
            darker: false,
            focus: None,
        }
    }

    /// The wait also covers a focus scan under the light (a lens that
    /// focuses by itself).
    pub fn with_focus(mut self) -> PreFlash {
        self.focus = Some(FocusWait::Due);
        self
    }

    /// Whether the focus scan under the light is still to come or under
    /// way.
    pub fn focusing(&self) -> bool {
        matches!(self.focus, Some(FocusWait::Due | FocusWait::Asked { .. }))
    }

    /// Whether a focus scan was asked for under the light.
    pub fn asked_focus(&self) -> bool {
        matches!(
            self.focus,
            Some(FocusWait::Asked { .. } | FocusWait::Done | FocusWait::GaveUp)
        )
    }

    /// Follow the focus scan: done once a frame after the scan says
    /// Focused or Failed, or when it takes too long.
    fn follow_focus(&mut self, meta: &FrameMeta, now: Instant) {
        let Some(FocusWait::Asked { at, scanning }) = &mut self.focus else {
            return;
        };
        let since = now.saturating_duration_since(*at);
        match meta.af_state {
            Some(AfState::Scanning) => *scanning = true,
            Some(AfState::Focused | AfState::Failed)
                if *scanning || since >= PRE_FLASH_FOCUS_SEEN =>
            {
                self.focus = Some(FocusWait::Done);
                return;
            }
            None if since >= PRE_FLASH_FOCUS_SEEN => {
                // The camera reports no focus state.
                self.focus = Some(FocusWait::Done);
                return;
            }
            _ => {}
        }
        if since >= PRE_FLASH_FOCUS {
            self.focus = Some(FocusWait::GaveUp);
        }
    }

    /// The same wait after the torch went out at `now`, `before` being the
    /// last frame under it: a photo taken at once would get the exposure
    /// set for the light (on the Moto G60s it came out dark). The exposure
    /// has seen the dark when the demand rises over the one before by as
    /// much as it falls for the light ([`PRE_FLASH_DROP`]).
    pub fn light_out(now: Instant, before: Option<&FrameMeta>) -> PreFlash {
        PreFlash {
            darker: true,
            ..PreFlash::new(now, before)
        }
    }

    /// A preview frame under the light.
    pub fn on_frame(&mut self, meta: &FrameMeta, now: Instant) -> PreFlashStep {
        self.frames += 1;
        let elapsed = now.saturating_duration_since(self.lit);
        let previous = std::mem::replace(&mut self.last_gains, meta.color_gains);
        let demand = FlashSpec::demand(meta);
        let last = std::mem::replace(&mut self.last_demand, demand);
        if elapsed >= PRE_FLASH_MAX {
            return PreFlashStep::TimedOut;
        }
        if self.reacted.is_none() {
            let dropped = match (demand, self.before) {
                (Some(d), Some(b)) if self.darker => d * PRE_FLASH_DROP > b,
                (Some(d), Some(b)) => d < b * PRE_FLASH_DROP,
                _ => false,
            };
            if dropped || meta.ae_state == Some(AeState::Searching) || elapsed >= PRE_FLASH_REACT {
                self.reacted = Some(now);
            } else {
                return PreFlashStep::Wait;
            }
        }
        // The light reaches the scene: the focus scan under it, while the
        // exposure walks there (the scan's measure doesn't depend on it).
        if self.focus == Some(FocusWait::Due) {
            self.focus = Some(FocusWait::Asked {
                at: now,
                scanning: false,
            });
            self.last_demand = demand;
            self.steady = 0;
            return PreFlashStep::Focus;
        }
        self.follow_focus(meta, now);
        let reacted = self.reacted.expect("just set");
        let steady = match (demand, last) {
            (Some(d), Some(l)) => l > 0.0 && ((d - l) / l).abs() <= PRE_FLASH_STEADY,
            (None, _) => true,
            (Some(_), None) => false,
        };
        self.steady = if steady { self.steady + 1 } else { 0 };
        if self.frames < PRE_FLASH_FRAMES || elapsed < PRE_FLASH_MIN {
            return PreFlashStep::Wait;
        }
        let color = match (meta.color_gains, previous) {
            (Some(got), Some(prev)) => got
                .iter()
                .zip(prev)
                .all(|(g, p)| p > 0.0 && ((g - p) / p).abs() as f64 <= PRE_FLASH_GAINS_STEADY),
            (Some(_), None) => false,
            // The camera reports no gains: nothing to wait for.
            (None, _) => true,
        };
        let exposure = match meta.ae_state {
            Some(AeState::Searching) => false,
            // Converged, or Idle (a locked or manual exposure).
            Some(_) => self.steady >= PRE_FLASH_STEADY_FRAMES,
            None => self.steady >= PRE_FLASH_STEADY_FRAMES && self.frames >= PRE_FLASH_FIXED,
        };
        // A focus scan under way holds the photo as long as it may take.
        let focus_until = match self.focus {
            Some(FocusWait::Asked { at, .. }) => Some(at + PRE_FLASH_FOCUS),
            _ => None,
        };
        if self.focus == Some(FocusWait::GaveUp) {
            return PreFlashStep::TimedOut;
        }
        if exposure && color && focus_until.is_none() {
            PreFlashStep::Settled
        } else if now.saturating_duration_since(reacted) >= PRE_FLASH_AFTER
            && focus_until.is_none_or(|t| now >= t)
        {
            PreFlashStep::TimedOut
        } else {
            PreFlashStep::Wait
        }
    }

    /// Time passes with no frame (a stalled stream): give up waiting.
    pub fn tick(&self, now: Instant) -> PreFlashStep {
        if now.saturating_duration_since(self.lit) >= PRE_FLASH_MAX {
            PreFlashStep::TimedOut
        } else {
            PreFlashStep::Wait
        }
    }

    /// Time since the light was asked for.
    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.lit)
    }

    /// Preview frames seen under the light.
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Whether the exposure has seen the light.
    pub fn reacted(&self) -> bool {
        self.reacted.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(exposure: i32, gain: f32, ae: Option<AeState>, gains: Option<[f32; 2]>) -> FrameMeta {
        FrameMeta {
            exposure_us: Some(exposure),
            analog_gain: Some(gain),
            ae_state: ae,
            color_gains: gains,
            ..FrameMeta::default()
        }
    }

    #[test]
    fn modes_parse_and_cycle() {
        for m in FlashMode::ALL {
            assert_eq!(m.name().parse::<FlashMode>(), Ok(m));
        }
        assert!("strobe".parse::<FlashMode>().is_err());
        let mut m = FlashMode::Off;
        let mut seen = vec![m];
        for _ in 0..3 {
            m = m.next();
            seen.push(m);
        }
        assert_eq!(seen, FlashMode::ALL);
        assert_eq!(m.next(), FlashMode::Off);
    }

    #[test]
    fn auto_fires_on_a_dim_preview() {
        let spec = FlashSpec {
            auto_above: 100_000.0,
            ..FlashSpec::default()
        };
        assert!(!spec.auto_fires(None));
        assert!(!spec.auto_fires(Some(&meta(10_000, 1.0, None, None))));
        assert!(!spec.auto_fires(Some(&meta(33_000, 3.0, None, None))));
        assert!(spec.auto_fires(Some(&meta(33_000, 4.0, None, None))));
        // Digital gain counts too.
        let m = FrameMeta {
            digital_gain: Some(2.0),
            ..meta(33_000, 2.0, None, None)
        };
        assert!(spec.auto_fires(Some(&m)));
        assert!(!spec.auto_fires(Some(&FrameMeta::default())));
    }

    #[test]
    fn exif_flash_says_whether_it_fired_and_why() {
        assert_eq!(exif_flash(false, FlashMode::On, false), 0x20);
        assert_eq!(exif_flash(true, FlashMode::On, true), 0x09);
        assert_eq!(exif_flash(true, FlashMode::Auto, true), 0x19);
        assert_eq!(exif_flash(true, FlashMode::Auto, false), 0x18);
        assert_eq!(exif_flash(true, FlashMode::Off, false), 0x10);
        assert_eq!(exif_flash(true, FlashMode::Torch, true), 0x01);
    }

    #[test]
    fn pre_flash_waits_for_the_exposure_to_see_the_light() {
        let now = Instant::now();
        let at = |ms: u64| now + Duration::from_millis(ms);
        // As on the Moto G60s: 100 ms at gain 15.5 before the light.
        let dark = meta(100_000, 15.5, Some(AeState::Converged), Some([2.0, 1.0]));
        let mut p = PreFlash::new(now, Some(&dark));
        // Converged, at the old exposure, past the least wait: the light
        // isn't seen yet, so it means nothing.
        for (i, ms) in [100, 200, 300, 400, 500].into_iter().enumerate() {
            assert_eq!(p.on_frame(&dark, at(ms)), PreFlashStep::Wait, "frame {i}");
        }
        assert!(!p.reacted());
        // The exposure comes down under the light; the white balance walks.
        let f = |t, g, ae, gains| meta(t, g, Some(ae), Some(gains));
        use AeState::*;
        assert_eq!(
            p.on_frame(&f(60_000, 8.0, Converged, [1.8, 1.2]), at(550)),
            PreFlashStep::Wait
        );
        assert!(p.reacted());
        assert_eq!(
            p.on_frame(&f(30_000, 4.0, Searching, [1.6, 1.4]), at(600)),
            PreFlashStep::Wait
        );
        assert_eq!(
            p.on_frame(&f(30_000, 3.0, Converged, [1.5, 1.5]), at(650)),
            PreFlashStep::Wait
        );
        assert_eq!(
            p.on_frame(&f(30_000, 2.9, Converged, [1.5, 1.5]), at(700)),
            PreFlashStep::Wait
        );
        assert_eq!(
            p.on_frame(&f(30_000, 2.9, Converged, [1.5, 1.51]), at(750)),
            PreFlashStep::Settled
        );
        assert_eq!(p.frames(), 10);
    }

    #[test]
    fn pre_flash_goes_ahead_a_while_after_the_exposure_saw_the_light() {
        // As the Moto G60s: the light seen after 400 ms, then a slow walk
        // that still says Searching.
        let now = Instant::now();
        let at = |ms: u64| now + Duration::from_millis(ms);
        let dark = meta(100_000, 15.5, Some(AeState::Converged), None);
        let mut p = PreFlash::new(now, Some(&dark));
        let mut ms = 0;
        let mut gain = 15.5;
        let step = loop {
            ms += 40;
            let m = if ms < 400 {
                dark.clone()
            } else {
                gain *= 0.97;
                meta(30_000, gain, Some(AeState::Searching), None)
            };
            match p.on_frame(&m, at(ms)) {
                PreFlashStep::Wait => {}
                step => break step,
            }
        };
        assert_eq!(step, PreFlashStep::TimedOut);
        let after = 400 + PRE_FLASH_AFTER.as_millis() as u64;
        assert!((after..after + 40).contains(&ms), "{ms}");
    }

    #[test]
    fn pre_flash_goes_on_when_the_light_changes_nothing() {
        // A bright or far scene: the exposure never moves.
        let now = Instant::now();
        let m = meta(10_000, 1.0, Some(AeState::Converged), None);
        let mut p = PreFlash::new(now, Some(&m));
        let mut settled = None;
        for i in 1..30 {
            if p.on_frame(&m, now + Duration::from_millis(50 * i)) == PreFlashStep::Settled {
                settled = Some(50 * i);
                break;
            }
        }
        let ms = settled.expect("settles");
        assert!(
            ms >= PRE_FLASH_REACT.as_millis() as u64 && ms < 1200,
            "{ms}"
        );
    }

    #[test]
    fn pre_flash_without_ae_state_waits_a_fixed_number_of_frames() {
        let now = Instant::now();
        let mut p = PreFlash::new(now, None);
        let m = meta(10_000, 1.0, None, None);
        let mut kept = None;
        for i in 1..40u64 {
            if p.on_frame(&m, now + Duration::from_millis(50 * i)) == PreFlashStep::Settled {
                kept = Some(i);
                break;
            }
        }
        // It waits out the reaction time, then the steady frames.
        let i = kept.expect("settles");
        assert!(
            i >= PRE_FLASH_FIXED as u64 && 50 * i >= PRE_FLASH_REACT.as_millis() as u64,
            "{i}"
        );
    }

    #[test]
    fn pre_flash_focuses_under_the_light_and_waits_for_the_scan() {
        let now = Instant::now();
        let at = |ms: u64| now + Duration::from_millis(ms);
        let dark = meta(100_000, 15.5, Some(AeState::Converged), Some([2.0, 1.0]));
        let lit = |af: AfState| FrameMeta {
            af_state: Some(af),
            ..meta(30_000, 3.0, Some(AeState::Converged), Some([1.5, 1.5]))
        };
        let mut p = PreFlash::new(now, Some(&dark)).with_focus();
        assert!(p.focusing() && !p.asked_focus());
        for ms in [100, 200, 300] {
            assert_eq!(p.on_frame(&dark, at(ms)), PreFlashStep::Wait);
        }
        // The exposure sees the light: ask for the scan, once.
        assert_eq!(
            p.on_frame(&lit(AfState::Focused), at(400)),
            PreFlashStep::Focus
        );
        assert!(p.asked_focus());
        // Exposure and color settle; the frames still in flight say
        // Focused, then the scan runs past the exposure's own wait.
        let mut ms = 400;
        for af in [AfState::Focused, AfState::Focused]
            .into_iter()
            .chain([AfState::Scanning; 20])
        {
            ms += 50;
            assert_eq!(p.on_frame(&lit(af), at(ms)), PreFlashStep::Wait, "{ms}");
        }
        assert!(ms > 400 + PRE_FLASH_AFTER.as_millis() as u64);
        assert!(p.focusing());
        // The lens stands at the result: the photo.
        assert_eq!(
            p.on_frame(&lit(AfState::Focused), at(ms + 50)),
            PreFlashStep::Settled
        );
        assert!(!p.focusing());

        // A scan that doesn't end holds the photo only so long.
        let mut p = PreFlash::new(now, Some(&dark)).with_focus();
        p.on_frame(&dark, at(100));
        assert_eq!(
            p.on_frame(&lit(AfState::Scanning), at(400)),
            PreFlashStep::Focus
        );
        let mut ms = 400;
        let step = loop {
            ms += 50;
            match p.on_frame(&lit(AfState::Scanning), at(ms)) {
                PreFlashStep::Wait => {}
                step => break step,
            }
        };
        assert_eq!(step, PreFlashStep::TimedOut);
        assert_eq!(ms, 400 + PRE_FLASH_FOCUS.as_millis() as u64);
    }

    #[test]
    fn pre_flash_gives_up_after_its_longest_wait() {
        let now = Instant::now();
        let mut p = PreFlash::new(now, None);
        let mut gain = 1.0;
        let mut searching = || {
            gain *= 1.5;
            meta(20_000, gain, Some(AeState::Searching), None)
        };
        assert_eq!(
            p.on_frame(&searching(), now + Duration::from_millis(1000)),
            PreFlashStep::Wait
        );
        assert_eq!(
            p.on_frame(&searching(), now + Duration::from_millis(2000)),
            PreFlashStep::Wait
        );
        assert_eq!(
            p.tick(now + Duration::from_millis(2400)),
            PreFlashStep::Wait
        );
        assert_eq!(p.tick(now + PRE_FLASH_MAX), PreFlashStep::TimedOut);
        assert_eq!(
            p.on_frame(&searching(), now + PRE_FLASH_MAX),
            PreFlashStep::TimedOut
        );
    }
}
