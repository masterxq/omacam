//! The controller driving the fake backend through the real command and
//! event path, in real time: open, zoom across lenses, take a full still,
//! flip, survive a busy camera, and light the flash.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use omacam_core::backend::{Backend, Event};
use omacam_core::controller::{Options, Output};
use omacam_core::flash::{FlashMode, FlashSpec, Light};
use omacam_core::lens::{Role, TableSpec};
use omacam_core::zoom::ZoomSource;
use omacam_core::{capture, Controller};
use omacam_fake::{FakeBackend, Options as FakeOptions};

struct Rig {
    backend: FakeBackend,
    events: mpsc::Receiver<Event>,
    controller: Controller,
    shown: Vec<String>,
    pictures: Vec<(String, f64, omacam_core::Size)>,
    errors: Vec<String>,
    /// Burst frames: lens, exposure and whether a raw picture came along.
    bursts: Vec<(String, Option<i32>, bool)>,
    bursts_done: u32,
    /// The LED as the controller set it, in order.
    lights: Vec<Light>,
    /// Each picture's exposure time times gain, and whether the LED was
    /// lit then.
    exposures: Vec<(Option<f64>, bool)>,
}

impl Rig {
    fn new(opts: FakeOptions) -> Rig {
        Rig::with(opts, Options::default())
    }

    fn with(opts: FakeOptions, controller_opts: Options) -> Rig {
        let (tx, events) = mpsc::channel();
        let backend = FakeBackend::spawn(
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
            opts,
        );
        let cameras = match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            Event::Cameras(c) => c,
            other => panic!("expected cameras, got {other:?}"),
        };
        let controller = Controller::new(&TableSpec::cheetah(), &cameras, controller_opts);
        Rig {
            backend,
            events,
            controller,
            shown: Vec::new(),
            pictures: Vec::new(),
            errors: Vec::new(),
            bursts: Vec::new(),
            bursts_done: 0,
            lights: Vec::new(),
            exposures: Vec::new(),
        }
    }

    fn apply(&mut self, out: Vec<Output>) {
        for o in out {
            match o {
                Output::Backend(c) => self.backend.send(c),
                Output::Show(f) => self.shown.push(f.camera),
                Output::Picture(p) => {
                    assert!(p.frame.raw.is_none(), "a single still takes no raw picture");
                    self.exposures
                        .push((FlashSpec::demand(&p.frame.meta), self.lit()));
                    self.pictures
                        .push((p.lens.name.clone(), p.crop, p.frame.size))
                }
                Output::BurstFrame(p) => self.bursts.push((
                    p.lens.name.clone(),
                    p.frame.meta.exposure_us,
                    p.frame.raw.is_some(),
                )),
                Output::BurstDone => self.bursts_done += 1,
                Output::Error(e) => self.errors.push(e),
                Output::Light(l) => {
                    self.backend.faults.lock().unwrap().flash_lit = l != Light::Off;
                    self.lights.push(l);
                }
            }
        }
    }

    fn lit(&self) -> bool {
        self.lights.last().is_some_and(|&l| l != Light::Off)
    }

    /// Pump events and ticks until `done` or the timeout.
    fn run_until(&mut self, timeout: Duration, done: impl Fn(&Rig) -> bool) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if done(self) {
                return true;
            }
            match self.events.recv_timeout(Duration::from_millis(10)) {
                // As the app does: take the new list and keep running.
                Ok(Event::Cameras(cams)) => {
                    let out =
                        self.controller
                            .set_cameras(&TableSpec::cheetah(), &cams, Instant::now());
                    self.apply(out);
                }
                Ok(e) => {
                    let out = self.controller.on_event(e, Instant::now());
                    self.apply(out);
                }
                Err(_) => {}
            }
            let out = self.controller.tick(Instant::now());
            self.apply(out);
        }
        done(self)
    }

    fn showing(&self, suffix: &str) -> bool {
        self.shown.last().is_some_and(|c| c.ends_with(suffix))
    }
}

const MAIN: &str = "@3d";
const UW: &str = "@1a";
const TELE: &str = "@2d";
const FRONT: &str = "@10";

fn fast() -> FakeOptions {
    FakeOptions {
        start_delay: Duration::from_millis(60),
        frame_interval: Duration::from_millis(10),
        max_size: omacam_core::Size::new(160, 120),
        ..FakeOptions::default()
    }
}

#[test]
fn zoom_across_all_lenses_and_back() {
    let mut rig = Rig::new(fast());
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(MAIN)));

    let out = rig
        .controller
        .set_zoom(5.0, ZoomSource::Preset, Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(TELE)));
    let switch = rig.controller.view().last_switch.unwrap();
    assert!(switch >= Duration::from_millis(60), "{switch:?}");

    // A pinch down to 0.6x: no switch until it settles, then the ultrawide.
    let now = Instant::now();
    let out = rig.controller.set_zoom(0.6, ZoomSource::Gesture, now);
    assert!(out.is_empty());
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(UW)));
    assert_eq!(rig.controller.view().lens_role, Some(Role::Ultrawide));
    assert!(rig.errors.is_empty(), "{:?}", rig.errors);
}

#[test]
fn a_night_burst_holds_one_exposure_and_the_preview_returns() {
    let mut rig = Rig::new(fast());
    let spec = capture::BurstSpec {
        frames: 4,
        under_stops: 0.0,
        max_exposure_us: 66_000,
    };
    // The phone's exposure loop ignores a stream's start controls: the
    // preview must get its automatic exposure back by request.
    rig.backend.faults.lock().unwrap().sticky_settings = true;
    rig.controller.set_capture_mode(capture::Mode::Burst(spec));
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(MAIN)));
    let out = rig.controller.shutter(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(5), |r| r.bursts_done == 1));
    assert!(rig.pictures.is_empty(), "a burst gives no single picture");
    assert_eq!(rig.bursts.len(), 4, "{:?}", rig.bursts);
    let first = rig.bursts[0].1;
    assert!(first.is_some());
    assert!(
        rig.bursts
            .iter()
            .all(|(lens, t, raw)| lens == "Main" && *t == first && *raw),
        "{:?}",
        rig.bursts
    );
    rig.shown.clear();
    assert!(
        rig.run_until(Duration::from_secs(2), |r| !r.controller.view().capturing
            && r.showing(MAIN))
    );
    assert!(rig.errors.is_empty(), "{:?}", rig.errors);
    // The burst's manual exposure doesn't stay on the preview.
    assert!(rig.run_until(Duration::from_secs(1), |r| {
        let kept = r.backend.faults.lock().unwrap().last_settings;
        kept.is_some_and(|s| s.shutter.is_auto() && s.gain.is_auto())
    }));
}

#[test]
fn full_still_comes_from_the_still_mode_and_the_preview_returns() {
    let mut rig = Rig::new(fast());
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(MAIN)));
    let out = rig
        .controller
        .set_zoom(2.0, ZoomSource::Preset, Instant::now());
    rig.apply(out);
    let out = rig.controller.shutter(Instant::now());
    rig.apply(out);
    assert!(rig.controller.view().capturing);
    assert!(rig.run_until(Duration::from_secs(3), |r| !r.pictures.is_empty()));
    let (lens, crop, size) = rig.pictures[0].clone();
    assert_eq!(lens, "Main");
    assert_eq!(crop, 2.0);
    // The still mode is 4:3 (4080x3072), the preview 16:9.
    let aspect = size.width as f64 / size.height as f64;
    assert!((aspect - 4.0 / 3.0).abs() < 0.02, "{size}");
    rig.shown.clear();
    assert!(
        rig.run_until(Duration::from_secs(2), |r| !r.controller.view().capturing
            && r.showing(MAIN))
    );
}

#[test]
fn a_still_without_memory_for_the_full_mode_falls_back_and_says_so() {
    let mut rig = Rig::new(fast());
    // Room for the preview (scaled to 160x120 here), not for the still.
    rig.backend.faults.lock().unwrap().memory_limit_pixels = Some(160 * 100);
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(MAIN)));
    let out = rig.controller.shutter(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(3), |r| !r.pictures.is_empty()));
    let (_, _, size) = rig.pictures[0].clone();
    let aspect = size.width as f64 / size.height as f64;
    assert!(
        (aspect - 16.0 / 9.0).abs() < 0.05,
        "the preview mode: {size}"
    );
    assert!(
        rig.errors
            .iter()
            .any(|e| e.contains("Not enough camera memory")),
        "{:?}",
        rig.errors
    );
}

#[test]
fn cameras_that_load_late_start_and_a_removed_one_hands_over() {
    // Opened before the camera drivers load (40 s into boot on the phone).
    let mut rig = Rig::new(FakeOptions {
        cameras: Vec::new(),
        ..fast()
    });
    assert!(!rig.controller.has_cameras());
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(!rig.run_until(Duration::from_millis(300), |r| !r.shown.is_empty()));
    assert!(
        rig.errors.is_empty(),
        "waiting is not an error: {:?}",
        rig.errors
    );
    // The drivers load.
    rig.backend.set_cameras(omacam_fake::cheetah_cameras());
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(MAIN)));
    // The main camera goes away (a driver reload): the stream stops and
    // the ultrawide, the widest left on the back, takes over.
    let mut left = omacam_fake::cheetah_cameras();
    left.retain(|c| !c.id.ends_with(MAIN));
    rig.backend.set_cameras(left);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(UW)));
    // All gone, then back: the main camera starts again.
    rig.backend.set_cameras(Vec::new());
    rig.run_until(Duration::from_millis(200), |_| false);
    assert!(!rig.controller.has_cameras());
    rig.backend.set_cameras(omacam_fake::cheetah_cameras());
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(MAIN)));
}

#[test]
fn flip_to_the_front_camera() {
    let mut rig = Rig::new(fast());
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(MAIN)));
    let out = rig.controller.flip(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(FRONT)));
}

#[test]
fn a_busy_camera_is_retried_until_it_frees() {
    let mut rig = Rig::new(fast());
    let main = omacam_fake::cheetah_cameras()[1].id.clone();
    rig.backend.faults.lock().unwrap().busy.insert(main.clone());
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(1), |r| !r.errors.is_empty()));
    rig.backend.faults.lock().unwrap().busy.remove(&main);
    assert!(rig.run_until(Duration::from_secs(4), |r| r.showing(MAIN)));
}

/// A dim room (the main camera needs 500 ms at gain 1, past auto's
/// threshold) with a flash that gives it 25 times the light.
fn dim_with_flash() -> FakeOptions {
    FakeOptions {
        scene_light: 0.02,
        flash_light: 0.5,
        ..fast()
    }
}

fn flash(mode: FlashMode) -> Options {
    Options {
        flash: true,
        flash_mode: mode,
        ..Options::default()
    }
}

/// Open on the main camera, take a photo, and wait until it's done.
fn photo(rig: &mut Rig) {
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(MAIN)));
    // Let the exposure settle on the dim scene.
    rig.run_until(Duration::from_millis(300), |_| false);
    let out = rig.controller.shutter(Instant::now());
    rig.apply(out);
    assert!(rig.controller.view().capturing);
    assert!(
        rig.run_until(Duration::from_secs(5), |r| !r.pictures.is_empty()
            && !r.controller.view().capturing)
    );
    assert!(rig.errors.is_empty(), "{:?}", rig.errors);
}

#[test]
fn a_flash_photo_lights_the_led_settles_under_it_and_puts_it_out() {
    let mut without = Rig::with(dim_with_flash(), flash(FlashMode::Off));
    photo(&mut without);
    assert!(without.lights.is_empty());
    let (dark, lit) = without.exposures[0];
    assert!(!lit);

    let mut rig = Rig::with(dim_with_flash(), flash(FlashMode::On));
    photo(&mut rig);
    assert_eq!(rig.lights, vec![Light::Photo, Light::Off]);
    let (t, lit) = rig.exposures[0];
    assert!(lit, "the picture is taken under the light");
    // 26 times the light: the exposure came down with it.
    assert!(
        t.unwrap() * 10.0 < dark.unwrap(),
        "with flash {t:?}, without {dark:?}"
    );
}

#[test]
fn auto_flash_fires_in_a_dim_scene_only() {
    let mut dim = Rig::with(dim_with_flash(), flash(FlashMode::Auto));
    photo(&mut dim);
    assert_eq!(dim.lights, vec![Light::Photo, Light::Off]);

    let bright = FakeOptions {
        scene_light: 1.0,
        ..dim_with_flash()
    };
    let mut rig = Rig::with(bright, flash(FlashMode::Auto));
    photo(&mut rig);
    assert!(rig.lights.is_empty(), "{:?}", rig.lights);
}

#[test]
fn the_torch_stays_on_through_a_photo_and_goes_out_on_the_front_and_when_put_away() {
    let mut rig = Rig::with(dim_with_flash(), flash(FlashMode::Torch));
    photo(&mut rig);
    assert_eq!(
        rig.lights,
        vec![Light::Torch],
        "lit once, through the photo"
    );
    assert!(rig.exposures[0].1);

    let out = rig.controller.flip(Instant::now());
    rig.apply(out);
    assert_eq!(
        rig.lights.last(),
        Some(&Light::Off),
        "the front has no flash"
    );
    assert!(!rig.controller.view().can_flash);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(FRONT)));
    let out = rig.controller.flip(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.lit() && r.showing(MAIN)));
    assert_eq!(rig.lights.last(), Some(&Light::Torch));

    let out = rig.controller.suspend(Instant::now());
    rig.apply(out);
    assert_eq!(rig.lights.last(), Some(&Light::Off));
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.lit()));
}

#[test]
fn a_busy_camera_puts_the_torch_out_until_it_streams_again() {
    let mut rig = Rig::with(dim_with_flash(), flash(FlashMode::Torch));
    let out = rig.controller.resume(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.lit()));
    // The front camera is busy: flipping there and back hits it on the
    // way; on the back side the main camera fails to restart.
    rig.backend
        .faults
        .lock()
        .unwrap()
        .busy
        .insert("/base/soc@0/usi@109000c0/i2c@10900000/camera-sensor@3d".into());
    let out = rig.controller.flip(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| r.showing(FRONT)));
    let out = rig.controller.flip(Instant::now());
    rig.apply(out);
    assert!(rig.run_until(Duration::from_secs(2), |r| !r.errors.is_empty()));
    assert!(!rig.lit(), "out while the camera fails");
    rig.backend.faults.lock().unwrap().busy.clear();
    assert!(rig.run_until(Duration::from_secs(4), |r| r.lit() && r.showing(MAIN)));
}
