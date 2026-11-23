//! The lens table: what each camera is, how it relates to the others, and
//! which modes to ask of it.
//!
//! A table is a TOML file (`data/cheetah.toml` for the Pixel 7 Pro). It
//! matches cameras by the library's id. Cameras the table doesn't know
//! still work: the first back camera becomes the main one and the first
//! front camera the selfie one, at the library's default mode.

use std::path::Path;

use serde::Deserialize;

use crate::backend::{CameraInfo, Location, Size};
use crate::flash::{FlashSpec, Led};

/// What a camera does in the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Ultrawide,
    Main,
    Tele,
    Front,
    /// A back camera that focuses at a few centimeters only. It stays out
    /// of the zoom ladder: the user turns it on and off
    /// ([`crate::Controller::toggle_macro`]).
    Macro,
}

impl Role {
    pub fn faces_front(self) -> bool {
        self == Role::Front
    }
}

/// One camera.
#[derive(Debug, Clone, PartialEq)]
pub struct Lens {
    pub role: Role,
    pub name: String,
    pub id: String,
    /// Field of view relative to the main camera (the front camera's ladder
    /// is its own, so its zoom is 1.0).
    pub zoom: f64,
    pub f_number: Option<f64>,
    pub focal_length_35mm: Option<u32>,
    /// The real focal length, millimeters.
    pub focal_length_mm: Option<f64>,
    pub preview: Option<Size>,
    pub still: Option<Size>,
    /// The size of the camera's own video stream, which video mode asks
    /// for instead of the preview (the frames feed the viewfinder and the
    /// recording alike). None: video records the preview's frames.
    pub video: Option<Size>,
    /// How far the lens zooms, relative to its own view: from `min_zoom`
    /// (1 unless the table says) up to `max_zoom` (None: as far as the
    /// table's `max_zoom` for the whole side reaches).
    pub min_zoom: f64,
    pub max_zoom: Option<f64>,
    /// Table says it focuses, and the camera has autofocus.
    pub autofocus: bool,
    pub af_windows: bool,
    pub crop_maximum: Option<crate::backend::Rect>,
    /// The camera's manual controls.
    pub caps: crate::backend::Caps,
    /// ISO at analog gain 1, for showing gain as ISO.
    pub base_iso: f64,
    pub mirror: bool,
    /// The camera's Rotation in libcamera's sense (correct by turning
    /// counter-clockwise). Filled from the camera's own Rotation property
    /// unless the table overrides it. See [`Lens::upright_clockwise`].
    pub rotation: u32,
    /// How much of the lens's whole field of view (its still mode's) the
    /// preview mode shows, along the frame's width and height: a 16:9
    /// mode of a 4:3 sensor keeps the width and crops the height. From the
    /// table, else derived from the preview and still sizes.
    pub coverage: (f64, f64),
    /// Where this lens's optical axis lands on the main camera's upright
    /// picture (0..1; the main camera's own is the center). The lenses sit
    /// a centimeter or two apart, so a lens switch shifts the crop by the
    /// difference to keep the subject still. From the table; the center
    /// until calibrated.
    pub axis: (f64, f64),
    /// Draw a preview that covers less than the whole picture at its true
    /// size inside it (`true_framing` in the table). Off, such a preview
    /// fills the picture as before, and frames the scene a little tighter
    /// than the lens's other modes and the other lenses: see
    /// [`Lens::shown_coverage`].
    pub true_framing: bool,
}

/// The share of a still mode's field of view that a preview mode shows,
/// along the frame's width and height. Sensors bin by a whole factor and
/// crop about the center, so the binning factor is the still width over
/// the preview width, rounded, and each side covers its binned size over
/// the still's.
pub fn derived_coverage(preview: Option<Size>, still: Option<Size>) -> (f64, f64) {
    let (Some(p), Some(s)) = (preview, still) else {
        return (1.0, 1.0);
    };
    if p.width == 0 || p.height == 0 || s.width == 0 || s.height == 0 {
        return (1.0, 1.0);
    }
    let bin = (s.width as f64 / p.width as f64).round().max(1.0);
    let side = |pv: u32, st: u32| (pv as f64 * bin / st as f64).min(1.0);
    (side(p.width, s.width), side(p.height, s.height))
}

/// Clockwise degrees that turn a frame upright, for a camera whose
/// libcamera `Rotation` is `rotation`. libcamera defines Rotation as the
/// counter-clockwise difference between the sensor's reference system and
/// the projected scene's, and says the image needs a correction of that
/// many degrees counter-clockwise: Rotation 90 means turning the frame
/// 90 degrees counter-clockwise, which is 270 clockwise.
pub fn upright_clockwise(rotation: u32) -> u32 {
    (360 - rotation % 360) % 360
}

impl Lens {
    /// The coverage the preview is drawn with: its real one where it
    /// covers the lens's whole picture or the table asks for true framing;
    /// otherwise the whole picture, so the preview fills the view with no
    /// empty bands (the main camera's 16:9 preview until it has a 4:3
    /// mode), at the cost of a tighter view than the photo's and a jump
    /// in framing at the switches to and from that lens.
    pub fn shown_coverage(&self) -> (f64, f64) {
        let full = self.coverage.0 >= 0.999 && self.coverage.1 >= 0.999;
        if full || self.true_framing {
            self.coverage
        } else {
            (1.0, 1.0)
        }
    }

    /// Clockwise degrees that turn this camera's frames upright.
    pub fn upright_clockwise(&self) -> u32 {
        upright_clockwise(self.rotation)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableFile {
    #[allow(dead_code)]
    device: Option<String>,
    max_zoom: Option<f64>,
    /// The app takes Night and HDR bursts of its own (see
    /// [`TableSpec::bursts`]); true when left out.
    bursts: Option<bool>,
    /// Photos keep the frame's pixels as they are and record the turn
    /// upright in EXIF (see [`TableSpec::exif_orientation`]).
    #[serde(default)]
    exif_orientation: bool,
    /// The name of a hardware JPEG encoder's V4L2 node (see
    /// [`TableSpec::jpeg_encoder`]).
    jpeg_encoder: Option<String>,
    /// The flash LED (see [`crate::flash`]).
    flash: Option<FlashEntry>,
    #[serde(default, rename = "lens")]
    lenses: Vec<LensEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlashEntry {
    /// The LED's name in /sys/class/leds; "" for no flash. Left out, the
    /// app looks for an LED whose name says flash or torch.
    led: Option<String>,
    torch: Option<u32>,
    photo: Option<u32>,
    auto_above: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LensEntry {
    role: Role,
    name: Option<String>,
    id: String,
    zoom: f64,
    f_number: Option<f64>,
    focal_length_35mm: Option<u32>,
    focal_length_mm: Option<f64>,
    base_iso: Option<f64>,
    preview: Option<[u32; 2]>,
    still: Option<[u32; 2]>,
    video: Option<[u32; 2]>,
    /// Zoom limits relative to the lens's own view.
    min_zoom: Option<f64>,
    max_zoom: Option<f64>,
    #[serde(default)]
    autofocus: bool,
    #[serde(default)]
    mirror: bool,
    rotation: Option<u32>,
    /// The preview mode's share of the field of view, [width, height] of
    /// the frame (see [`Lens::coverage`]).
    coverage: Option<[f64; 2]>,
    /// Where the optical axis lands on the main camera's upright picture.
    axis: Option<[f64; 2]>,
    #[serde(default)]
    true_framing: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TableError {
    #[error("lens table: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("lens table: {0}")]
    Invalid(String),
    #[error("lens table {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
}

/// The lens table as written, before it meets the cameras.
#[derive(Debug, Clone)]
pub struct TableSpec {
    max_zoom: f64,
    bursts: bool,
    exif_orientation: bool,
    jpeg_encoder: Option<String>,
    flash: FlashSpec,
    entries: Vec<LensEntry>,
}

/// The built-in table for the Pixel 7 Pro.
pub const CHEETAH: &str = include_str!("../../../data/cheetah.toml");
/// The built-in table for the Pixel 9a.
pub const TEGU: &str = include_str!("../../../data/tegu.toml");
/// The built-in table for the Moto G60s.
pub const LISBON: &str = include_str!("../../../data/lisbon.toml");

/// Highest zoom when the table doesn't say.
pub const DEFAULT_MAX_ZOOM: f64 = 8.0;
/// ISO at gain 1 when the table doesn't say: phone sensors are about
/// 50 at their base gain.
pub const DEFAULT_BASE_ISO: f64 = 50.0;
/// Highest digital zoom of the front camera.
pub const FRONT_MAX_ZOOM: f64 = 4.0;
/// Highest digital zoom of the macro camera.
pub const MACRO_MAX_ZOOM: f64 = 4.0;

impl TableSpec {
    pub fn parse(text: &str) -> Result<TableSpec, TableError> {
        let file: TableFile = toml::from_str(text)?;
        for e in &file.lenses {
            if !(e.zoom.is_finite() && e.zoom > 0.0) {
                return Err(TableError::Invalid(format!("{}: zoom must be > 0", e.id)));
            }
            let lo = e.min_zoom.unwrap_or(1.0);
            if !(lo.is_finite()
                && lo >= 1.0
                && e.max_zoom.is_none_or(|hi| hi.is_finite() && hi >= lo))
            {
                return Err(TableError::Invalid(format!(
                    "{}: min_zoom must be >= 1 and max_zoom at least min_zoom",
                    e.id
                )));
            }
            let unit = |v: Option<[f64; 2]>, lo: f64| {
                v.is_none_or(|[a, b]| {
                    a.is_finite() && b.is_finite() && a > lo && b > lo && a <= 1.0 && b <= 1.0
                })
            };
            if !unit(e.coverage, 0.0) {
                return Err(TableError::Invalid(format!(
                    "{}: coverage must be in (0, 1]",
                    e.id
                )));
            }
            if !unit(e.axis, -1.0) || e.axis.is_some_and(|[a, b]| a < 0.0 || b < 0.0) {
                return Err(TableError::Invalid(format!(
                    "{}: axis must be in [0, 1]",
                    e.id
                )));
            }
            if let Some(r) = e.rotation {
                if r % 90 != 0 || r >= 360 {
                    return Err(TableError::Invalid(format!(
                        "{}: rotation must be 0, 90, 180 or 270",
                        e.id
                    )));
                }
            }
        }
        let max_zoom = file.max_zoom.unwrap_or(DEFAULT_MAX_ZOOM);
        if !(max_zoom.is_finite() && max_zoom >= 1.0) {
            return Err(TableError::Invalid("max_zoom must be >= 1".into()));
        }
        let flash = match file.flash {
            None => FlashSpec::default(),
            Some(f) => {
                let d = FlashSpec::default();
                let spec = FlashSpec {
                    led: match f.led {
                        None => Led::Find,
                        Some(name) if name.is_empty() => Led::None,
                        Some(name) => Led::Named(name),
                    },
                    torch: f.torch,
                    photo: f.photo,
                    auto_above: f.auto_above.unwrap_or(d.auto_above),
                };
                if !(spec.auto_above.is_finite() && spec.auto_above > 0.0) {
                    return Err(TableError::Invalid("flash: auto_above must be > 0".into()));
                }
                if [spec.torch, spec.photo].contains(&Some(0)) {
                    return Err(TableError::Invalid(
                        "flash: a brightness must be at least 1".into(),
                    ));
                }
                spec
            }
        };
        Ok(TableSpec {
            max_zoom,
            bursts: file.bursts.unwrap_or(true),
            exif_orientation: file.exif_orientation,
            jpeg_encoder: file.jpeg_encoder.filter(|n| !n.is_empty()),
            flash,
            entries: file.lenses,
        })
    }

    pub fn load(path: &Path) -> Result<TableSpec, TableError> {
        let text = std::fs::read_to_string(path).map_err(|source| TableError::Read {
            path: path.display().to_string(),
            source,
        })?;
        TableSpec::parse(&text)
    }

    pub fn cheetah() -> TableSpec {
        TableSpec::parse(CHEETAH).expect("built-in lens table parses")
    }

    pub fn tegu() -> TableSpec {
        TableSpec::parse(TEGU).expect("built-in lens table parses")
    }

    pub fn lisbon() -> TableSpec {
        TableSpec::parse(LISBON).expect("built-in lens table parses")
    }

    /// The built-in table for the device whose device-tree `compatible`
    /// strings are given (the Pixel 7 Pro's when none matches).
    pub fn builtin(compatible: &[&str]) -> TableSpec {
        if compatible.contains(&"google,zumapro-tegu") {
            TableSpec::tegu()
        } else if compatible.contains(&"motorola,lisbon") {
            TableSpec::lisbon()
        } else {
            TableSpec::cheetah()
        }
    }

    /// An empty table: every camera gets a generic entry.
    pub fn empty() -> TableSpec {
        TableSpec {
            max_zoom: DEFAULT_MAX_ZOOM,
            bursts: true,
            exif_orientation: false,
            jpeg_encoder: None,
            flash: FlashSpec::default(),
            entries: Vec::new(),
        }
    }

    /// Whether the app may take Night and HDR bursts of its own: a burst
    /// holds one exposure of the app's over several frames. False where
    /// the camera merges a series of frames of its own for each photo,
    /// at an exposure of its own: the app's Night mode and HDR then can't
    /// work and stay hidden.
    pub fn bursts(&self) -> bool {
        self.bursts
    }

    /// Whether a photo keeps the frame's pixels as the camera gave them
    /// and records the turn that makes it upright (and the front camera's
    /// mirror) as its EXIF Orientation, instead of turning the pixels:
    /// cheaper, and what a hardware JPEG encoder needs, but a viewer that
    /// ignores the tag shows the photo sideways. False when left out.
    pub fn exif_orientation(&self) -> bool {
        self.exif_orientation
    }

    /// The name of the V4L2 memory-to-memory JPEG encoder to encode photos
    /// with (as /sys/class/video4linux/*/name says it), or None to encode
    /// them on the CPU. Photos fall back to the CPU when no such node is
    /// there or it fails.
    pub fn jpeg_encoder(&self) -> Option<&str> {
        self.jpeg_encoder.as_deref()
    }

    /// The flash LED: which one, how bright, and when auto fires.
    pub fn flash(&self) -> &FlashSpec {
        &self.flash
    }

    /// Match the table against the cameras present.
    pub fn resolve(&self, cameras: &[CameraInfo]) -> LensTable {
        let mut lenses = Vec::new();
        for cam in cameras {
            if let Some(e) = self.entries.iter().find(|e| e.id == cam.id) {
                lenses.push(Lens {
                    role: e.role,
                    name: e.name.clone().unwrap_or_else(|| cam.model.clone()),
                    id: cam.id.clone(),
                    zoom: e.zoom,
                    f_number: e.f_number,
                    focal_length_35mm: e.focal_length_35mm,
                    focal_length_mm: e.focal_length_mm,
                    preview: e.preview.map(|[w, h]| Size::new(w, h)),
                    still: e.still.map(|[w, h]| Size::new(w, h)),
                    video: e.video.map(|[w, h]| Size::new(w, h)),
                    min_zoom: e.min_zoom.unwrap_or(1.0),
                    max_zoom: e.max_zoom,
                    autofocus: e.autofocus && cam.autofocus,
                    af_windows: cam.af_windows,
                    crop_maximum: cam.crop_maximum,
                    caps: cam.caps.clone(),
                    base_iso: e.base_iso.unwrap_or(DEFAULT_BASE_ISO),
                    mirror: e.mirror,
                    rotation: e.rotation.unwrap_or(cam.rotation % 360),
                    coverage: e.coverage.map(|[a, b]| (a, b)).unwrap_or_else(|| {
                        derived_coverage(
                            e.preview.map(|[w, h]| Size::new(w, h)),
                            e.still.map(|[w, h]| Size::new(w, h)),
                        )
                    }),
                    axis: e.axis.map(|[a, b]| (a, b)).unwrap_or((0.5, 0.5)),
                    true_framing: e.true_framing,
                });
            }
        }
        // Cameras the table doesn't know: one main and one front, at most.
        for cam in cameras {
            if lenses.iter().any(|l| l.id == cam.id) {
                continue;
            }
            let role = match cam.location {
                Location::Front => Role::Front,
                Location::Back | Location::External => Role::Main,
            };
            if lenses.iter().any(|l| l.role == role) {
                continue;
            }
            lenses.push(Lens {
                role,
                name: cam.model.clone(),
                id: cam.id.clone(),
                zoom: 1.0,
                f_number: None,
                focal_length_35mm: None,
                focal_length_mm: None,
                preview: None,
                still: None,
                video: None,
                min_zoom: 1.0,
                max_zoom: None,
                autofocus: cam.autofocus,
                af_windows: cam.af_windows,
                crop_maximum: cam.crop_maximum,
                caps: cam.caps.clone(),
                base_iso: DEFAULT_BASE_ISO,
                mirror: role == Role::Front,
                rotation: cam.rotation % 360,
                coverage: (1.0, 1.0),
                axis: (0.5, 0.5),
                true_framing: false,
            });
        }
        LensTable {
            lenses,
            max_zoom: self.max_zoom,
            bursts: self.bursts,
            flash: self.flash.clone(),
        }
    }
}

/// The lenses present on this device.
#[derive(Debug, Clone, PartialEq)]
pub struct LensTable {
    lenses: Vec<Lens>,
    max_zoom: f64,
    bursts: bool,
    flash: FlashSpec,
}

/// Which way the app looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    Back,
    Front,
}

impl LensTable {
    pub fn lenses(&self) -> &[Lens] {
        &self.lenses
    }

    pub fn get(&self, index: usize) -> &Lens {
        &self.lenses[index]
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.lenses.iter().position(|l| l.id == id)
    }

    pub fn by_role(&self, role: Role) -> Option<usize> {
        self.lenses.iter().position(|l| l.role == role)
    }

    pub fn has(&self, facing: Facing) -> bool {
        !self.ladder(facing).is_empty()
    }

    /// The lenses of one side, ordered by zoom: the indices into
    /// [`LensTable::lenses`]. The macro lens isn't on it.
    pub fn ladder(&self, facing: Facing) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.lenses.len())
            .filter(|&i| self.lenses[i].role != Role::Macro)
            .filter(|&i| (self.lenses[i].role == Role::Front) == (facing == Facing::Front))
            .collect();
        idx.sort_by(|&a, &b| self.lenses[a].zoom.total_cmp(&self.lenses[b].zoom));
        idx
    }

    pub fn max_zoom(&self, facing: Facing) -> f64 {
        match facing {
            Facing::Back => self.max_zoom,
            Facing::Front => FRONT_MAX_ZOOM,
        }
    }

    /// Whether the app may take bursts of its own ([`TableSpec::bursts`]).
    pub fn bursts(&self) -> bool {
        self.bursts
    }

    /// The flash LED ([`TableSpec::flash`]).
    pub fn flash(&self) -> &FlashSpec {
        &self.flash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn cheetah_cameras() -> Vec<CameraInfo> {
        let cam = |id: &str, location, rotation| CameraInfo {
            id: id.into(),
            location,
            rotation,
            model: "sensor".into(),
            autofocus: true,
            ..CameraInfo::default()
        };
        vec![
            cam(
                "/base/soc@0/usi@109100c0/i2c@10910000/camera-sensor@10",
                Location::Front,
                90,
            ),
            cam(
                "/base/soc@0/usi@109000c0/i2c@10900000/camera-sensor@3d",
                Location::Back,
                90,
            ),
            cam(
                "/base/soc@0/usi@109200c0/i2c@10920000/camera-sensor@1a",
                Location::Back,
                270,
            ),
            cam(
                "/base/soc@0/usi@109300c0/i2c@10930000/camera-sensor@2d",
                Location::Back,
                270,
            ),
        ]
    }

    #[test]
    fn tegu_table_matches_its_three_cameras() {
        let cam = |id: &str, location| CameraInfo {
            id: id.into(),
            location,
            rotation: 0,
            model: "sensor".into(),
            ..CameraInfo::default()
        };
        let cams = vec![
            cam(
                "/base/soc/usi@108c00c0/i2c@108c0000/camera@56",
                Location::Back,
            ),
            cam(
                "/base/soc/usi@108900c0/i2c@10890000/camera@1a",
                Location::Back,
            ),
            cam(
                "/base/soc/usi@108a00c0/i2c@108a0000/camera@1a",
                Location::Front,
            ),
        ];
        let t = TableSpec::builtin(&["google,zumapro-tegu", "google,zumapro"]).resolve(&cams);
        assert_eq!(t.lenses().len(), 3);
        for role in [Role::Main, Role::Ultrawide, Role::Front] {
            assert!(t.by_role(role).is_some(), "{role:?}");
        }
    }

    #[test]
    fn lisbon_table_matches_its_two_cameras() {
        let cam = |id: &str, location, rotation| CameraInfo {
            id: id.into(),
            location,
            rotation,
            model: "sensor".into(),
            autofocus: true,
            ..CameraInfo::default()
        };
        let cams = vec![
            cam("/base/soc/i2c@11009000/camera@22", Location::Back, 90),
            cam("/base/soc/i2c@11011000/camera@10", Location::Front, 270),
        ];
        let t = TableSpec::builtin(&["motorola,lisbon", "mediatek,mt6785"]).resolve(&cams);
        assert_eq!(t.lenses().len(), 2);
        let main = t.get(t.by_role(Role::Main).expect("main"));
        assert_eq!(main.still, Some(Size::new(4624, 3472)));
        assert_eq!(main.rotation, 90);
        assert!(main.autofocus);
        assert_eq!(main.shown_coverage(), (1.0, 1.0));
        let front = t.get(t.by_role(Role::Front).expect("front"));
        assert!(front.mirror && !front.autofocus);
        assert_eq!(t.max_zoom(Facing::Back), 10.0);
        // Its camera merges a series of frames for each photo itself.
        assert!(!t.bursts());
    }

    #[test]
    fn bursts_are_on_unless_the_table_turns_them_off() {
        for spec in [TableSpec::cheetah(), TableSpec::tegu(), TableSpec::empty()] {
            assert!(spec.bursts());
            assert!(spec.resolve(&cheetah_cameras()).bursts());
        }
        assert!(!TableSpec::lisbon().bursts());
        let off = TableSpec::parse("bursts = false\n").unwrap();
        assert!(!off.bursts() && !off.resolve(&[]).bursts());
        assert!(TableSpec::parse("bursts = \"no\"\n").is_err());
    }

    #[test]
    fn exif_orientation_and_the_jpeg_encoder_are_off_unless_the_table_says() {
        for spec in [TableSpec::cheetah(), TableSpec::tegu(), TableSpec::empty()] {
            assert!(!spec.exif_orientation());
            assert_eq!(spec.jpeg_encoder(), None);
        }
        assert!(TableSpec::lisbon().exif_orientation());
        let t = TableSpec::parse("exif_orientation = true\njpeg_encoder = \"enc\"\n").unwrap();
        assert!(t.exif_orientation());
        assert_eq!(t.jpeg_encoder(), Some("enc"));
        let none = TableSpec::parse("jpeg_encoder = \"\"\n").unwrap();
        assert_eq!(none.jpeg_encoder(), None);
    }

    #[test]
    fn the_flash_entry() {
        // Left out: look for an LED, at its full brightness.
        let any = TableSpec::empty();
        assert_eq!(any.flash(), &FlashSpec::default());
        assert_eq!(any.flash().led, Led::Find);
        assert_eq!(TableSpec::cheetah().flash().led, Led::Find);
        let lisbon = TableSpec::lisbon();
        let f = lisbon.flash();
        assert_eq!(f.led, Led::Named("white:flash-1".into()));
        assert_eq!((f.torch, f.photo), (Some(13), Some(13)));
        assert_eq!(lisbon.resolve(&[]).flash(), f);
        let none = TableSpec::parse("[flash]\nled = \"\"\n").unwrap();
        assert_eq!(none.flash().led, Led::None);
        for bad in [
            "[flash]\nauto_above = 0\n",
            "[flash]\nthermal_zone = \"x\"\n",
            "[flash]\ntorch = 0\n",
            "[flash]\nstrobe = 1\n",
        ] {
            assert!(TableSpec::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn builtin_picks_cheetah_elsewhere() {
        let t = TableSpec::builtin(&["google,gs201-cheetah", "google,gs201"])
            .resolve(&cheetah_cameras());
        assert_eq!(t.lenses().len(), 4);
    }

    #[test]
    fn builtin_table_matches_all_four_cameras() {
        let t = TableSpec::cheetah().resolve(&cheetah_cameras());
        assert_eq!(t.lenses().len(), 4);
        let roles: Vec<Role> = t
            .ladder(Facing::Back)
            .iter()
            .map(|&i| t.get(i).role)
            .collect();
        assert_eq!(roles, [Role::Ultrawide, Role::Main, Role::Tele]);
        assert_eq!(t.ladder(Facing::Front).len(), 1);
        let uw = t.get(t.by_role(Role::Ultrawide).unwrap());
        assert_eq!(uw.zoom, 0.45);
        assert_eq!(uw.rotation, 270, "rotation comes from the camera");
        assert_eq!(uw.still, Some(Size::new(4032, 3016)), "the full mode");
        let tele = t.get(t.by_role(Role::Tele).unwrap());
        assert_eq!(tele.zoom, 4.78);
        assert!(!tele.autofocus);
        assert!(t.get(t.by_role(Role::Front).unwrap()).mirror);
        assert_eq!(t.max_zoom(Facing::Back), 15.0);
    }

    /// What the Pixel 7 Pro's cameras report (`omacam --list`, 2026-10-14,
    /// kernel 00545) and how far each frame must turn clockwise to be
    /// upright: main and front Rotation 90, ultrawide and telephoto 270.
    #[test]
    fn cheetah_frames_turn_upright_counter_clockwise_by_rotation() {
        let t = TableSpec::cheetah().resolve(&cheetah_cameras());
        let cw = |role| t.get(t.by_role(role).unwrap()).upright_clockwise();
        assert_eq!(cw(Role::Main), 270);
        assert_eq!(cw(Role::Ultrawide), 90);
        assert_eq!(cw(Role::Tele), 90);
        assert_eq!(cw(Role::Front), 270);
    }

    #[test]
    fn upright_clockwise_inverts_libcamera_rotation() {
        assert_eq!(upright_clockwise(0), 0);
        assert_eq!(upright_clockwise(90), 270);
        assert_eq!(upright_clockwise(180), 180);
        assert_eq!(upright_clockwise(270), 90);
        assert_eq!(upright_clockwise(360), 0);
    }

    #[test]
    fn coverage_is_derived_from_the_modes() {
        let t = TableSpec::cheetah().resolve(&cheetah_cameras());
        let cov = |role| t.get(t.by_role(role).unwrap()).coverage;
        // The main camera's 2016x1136 is the 2x2-binned center of its
        // 4080x3072: nearly the whole width, 74 % of the height.
        let (w, h) = cov(Role::Main);
        assert!((w - 4032.0 / 4080.0).abs() < 1e-9, "{w}");
        assert!((h - 2272.0 / 3072.0).abs() < 1e-9, "{h}");
        assert_eq!(cov(Role::Ultrawide), (1.0, 1.0));
        // Shown filling the view until the main camera has a 4:3 preview.
        let main = t.get(t.by_role(Role::Main).unwrap());
        assert_eq!(main.shown_coverage(), (1.0, 1.0));
        let truly = Lens {
            true_framing: true,
            ..main.clone()
        };
        assert_eq!(truly.shown_coverage(), main.coverage);
        assert_eq!(cov(Role::Tele), (1.0, 1.0));
        assert_eq!(derived_coverage(None, Some(Size::new(4, 3))), (1.0, 1.0));
        assert_eq!(
            derived_coverage(Some(Size::new(640, 480)), Some(Size::new(640, 480))),
            (1.0, 1.0)
        );
    }

    #[test]
    fn coverage_and_axis_from_the_table_are_checked() {
        let base = "[[lens]]\nrole = \"main\"\nid = \"a\"\nzoom = 1.0\n";
        let cams = vec![CameraInfo {
            id: "a".into(),
            ..CameraInfo::default()
        }];
        let t = TableSpec::parse(&format!(
            "{base}coverage = [1.0, 0.75]\naxis = [0.52, 0.49]\n"
        ))
        .unwrap()
        .resolve(&cams);
        assert_eq!(t.get(0).coverage, (1.0, 0.75));
        assert_eq!(t.get(0).axis, (0.52, 0.49));
        assert!(TableSpec::parse(&format!("{base}coverage = [0.0, 1.0]\n")).is_err());
        assert!(TableSpec::parse(&format!("{base}coverage = [1.2, 1.0]\n")).is_err());
        assert!(TableSpec::parse(&format!("{base}axis = [-0.1, 0.5]\n")).is_err());
    }

    #[test]
    fn missing_cameras_drop_out_of_the_ladder() {
        let mut cams = cheetah_cameras();
        cams.retain(|c| !c.id.ends_with("@2d"));
        let t = TableSpec::cheetah().resolve(&cams);
        assert_eq!(t.ladder(Facing::Back).len(), 2);
        assert!(t.by_role(Role::Tele).is_none());
    }

    #[test]
    fn unknown_cameras_get_generic_roles() {
        let cams = vec![
            CameraInfo {
                id: "usb-1".into(),
                location: Location::External,
                rotation: 0,
                model: "Webcam".into(),
                autofocus: true,
                ..CameraInfo::default()
            },
            CameraInfo {
                id: "usb-2".into(),
                location: Location::External,
                rotation: 0,
                model: "Second".into(),
                autofocus: true,
                ..CameraInfo::default()
            },
        ];
        let t = TableSpec::cheetah().resolve(&cams);
        assert_eq!(t.lenses().len(), 1, "only one generic main camera");
        assert_eq!(t.get(0).role, Role::Main);
        assert_eq!(t.get(0).name, "Webcam");
        assert!(!t.has(Facing::Front));
    }

    #[test]
    fn table_override_of_rotation_and_validation() {
        let spec =
            TableSpec::parse("[[lens]]\nrole = \"main\"\nid = \"a\"\nzoom = 1.0\nrotation = 180\n")
                .unwrap();
        let cams = vec![CameraInfo {
            id: "a".into(),
            location: Location::Back,
            rotation: 90,
            model: "m".into(),
            autofocus: true,
            ..CameraInfo::default()
        }];
        assert_eq!(spec.resolve(&cams).get(0).rotation, 180);
        assert!(TableSpec::parse("[[lens]]\nrole = \"main\"\nid = \"a\"\nzoom = 0\n").is_err());
        assert!(TableSpec::parse(
            "[[lens]]\nrole = \"main\"\nid = \"a\"\nzoom = 1\nrotation = 45\n"
        )
        .is_err());
        assert!(TableSpec::parse("[[lens]]\nrole = \"side\"\nid = \"a\"\nzoom = 1\n").is_err());
    }

    #[test]
    fn the_lisbon_table_has_a_macro_lens_beside_the_ladder() {
        let cam = |id: &str, location| CameraInfo {
            id: id.into(),
            location,
            rotation: 270,
            model: "sensor".into(),
            ..CameraInfo::default()
        };
        let cams = vec![
            cam("/base/soc/i2c@11009000/camera@22", Location::Back),
            cam("/base/soc/i2c@11011000/camera@10", Location::Front),
            cam("/base/soc/i2c@1101a000/camera@10", Location::Back),
        ];
        let t = TableSpec::builtin(&["motorola,lisbon", "mediatek,mt6785"]).resolve(&cams);
        assert_eq!(t.lenses().len(), 3);
        let m = t.get(t.by_role(Role::Macro).expect("macro"));
        assert_eq!(m.id, "/base/soc/i2c@1101a000/camera@10");
        assert!(!m.autofocus && !m.mirror);
        assert_eq!(m.still, Some(Size::new(2592, 1940)));
        let main = t.by_role(Role::Main).unwrap();
        assert_eq!(t.ladder(Facing::Back), vec![main]);
    }

    #[test]
    fn the_lisbon_table_has_an_ultrawide_below_the_main_camera() {
        let cam = |id: &str, location| CameraInfo {
            id: id.into(),
            location,
            rotation: 270,
            model: "sensor".into(),
            ..CameraInfo::default()
        };
        let cams = vec![
            cam("/base/soc/i2c@11009000/camera@22", Location::Back),
            cam("/base/soc/i2c@11011000/camera@10", Location::Front),
            cam("/base/soc/i2c@1101a000/camera@2d", Location::Back),
            cam("/base/soc/i2c@1101a000/camera@10", Location::Back),
        ];
        let t = TableSpec::builtin(&["motorola,lisbon", "mediatek,mt6785"]).resolve(&cams);
        assert_eq!(t.lenses().len(), 4);
        let uw = t.by_role(Role::Ultrawide).expect("ultrawide");
        let main = t.by_role(Role::Main).unwrap();
        assert_eq!(t.ladder(Facing::Back), vec![uw, main]);
        let l = t.get(uw);
        assert_eq!(l.name, "Wide");
        assert!(l.zoom > 0.4 && l.zoom < 0.7);
        assert_eq!(l.max_zoom, Some(8.0));
        assert!(!l.autofocus && !l.mirror);
        assert_eq!(l.still, Some(Size::new(3234, 2448)));
        assert_eq!(l.shown_coverage(), (1.0, 1.0));
    }
}
