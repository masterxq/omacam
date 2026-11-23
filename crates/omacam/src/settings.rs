//! The user's settings, kept in `~/.config/omacam/settings.toml`.
//!
//! Every field has a default, and unknown or missing keys fall back to it,
//! so a settings file from an older or newer omacam still loads. A file
//! that doesn't parse is left alone (renamed aside) rather than lost.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use omacam_core::flash::FlashMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Aspect {
    /// The whole frame of the lens's mode.
    #[default]
    Full,
    #[serde(rename = "4:3")]
    FourThree,
    #[serde(rename = "16:9")]
    SixteenNine,
    #[serde(rename = "1:1")]
    Square,
}

impl Aspect {
    pub const ALL: [Aspect; 4] = [
        Aspect::Full,
        Aspect::FourThree,
        Aspect::SixteenNine,
        Aspect::Square,
    ];

    /// Width over height of an upright (portrait) photo, None for full.
    pub fn portrait_ratio(self) -> Option<f64> {
        match self {
            Aspect::Full => None,
            Aspect::FourThree => Some(3.0 / 4.0),
            Aspect::SixteenNine => Some(9.0 / 16.0),
            Aspect::Square => Some(1.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    /// The lens's full-resolution still mode.
    #[default]
    Full,
    /// Half the width and height: a quarter of the pixels.
    Medium,
}

impl Resolution {
    pub const ALL: [Resolution; 2] = [Resolution::Full, Resolution::Medium];

    pub fn label(self) -> &'static str {
        match self {
            Resolution::Full => "Full",
            Resolution::Medium => "Medium (a quarter of the pixels)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Timer {
    #[default]
    Off,
    #[serde(rename = "3s")]
    Three,
    #[serde(rename = "10s")]
    Ten,
}

impl Timer {
    pub fn seconds(self) -> u32 {
        match self {
            Timer::Off => 0,
            Timer::Three => 3,
            Timer::Ten => 10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Grid {
    #[default]
    Off,
    #[serde(rename = "3x3")]
    Thirds,
    #[serde(rename = "4x4")]
    Quarters,
}

impl Grid {
    pub const ALL: [Grid; 3] = [Grid::Off, Grid::Thirds, Grid::Quarters];

    pub fn lines(self) -> u32 {
        match self {
            Grid::Off => 0,
            Grid::Thirds => 3,
            Grid::Quarters => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Grid::Off => "Off",
            Grid::Thirds => "3 × 3",
            Grid::Quarters => "4 × 4",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VideoResolution {
    #[default]
    #[serde(rename = "1080p")]
    FullHd,
    #[serde(rename = "720p")]
    Hd,
}

impl VideoResolution {
    pub const ALL: [VideoResolution; 2] = [VideoResolution::FullHd, VideoResolution::Hd];

    /// Landscape width and height of the encoded frames.
    pub fn size(self) -> (u32, u32) {
        match self {
            VideoResolution::FullHd => (1920, 1080),
            VideoResolution::Hd => (1280, 720),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            VideoResolution::FullHd => "1080p",
            VideoResolution::Hd => "720p (lighter on the battery)",
        }
    }
}

pub const DEFAULT_QUALITY: u8 = 92;

/// The settings of the gear's sheet, kept in `settings.toml`, and the
/// quick settings of the top bar, which every launch starts at their
/// defaults (`#[serde(skip)]`: neither saved nor read), so nothing stays
/// set by accident: the aspect ratio, the self-timer and the flash. The
/// manual values, the lens and the zoom start afresh too (they live in the
/// controller).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(skip)]
    pub aspect: Aspect,
    pub resolution: Resolution,
    /// JPEG quality, 50-100.
    pub jpeg_quality: u8,
    #[serde(skip)]
    pub timer: Timer,
    pub grid: Grid,
    /// Save front-camera photos mirrored, as the preview shows them.
    pub mirror_front: bool,
    /// Location tags in photos and videos, from GeoClue (off by default,
    /// CAMERA-APP-PLAN Q5).
    pub location: bool,
    /// The shutter sound (on by default, Q7).
    pub shutter_sound: bool,
    /// Where photos go; None for `$XDG_PICTURES_DIR/Camera`.
    pub save_dir: Option<PathBuf>,
    /// Volume keys take a photo while the camera has focus.
    pub volume_shutter: bool,
    pub tap_to_focus: bool,
    pub debug_overlay: bool,
    pub video_resolution: VideoResolution,
    /// In dim light, show the telephoto's zoom with the main camera.
    pub low_light: bool,
    /// Video frame rate: 30, or 60 where the lens's mode allows it.
    pub video_fps: u32,
    /// Steady videos with the gyroscope, where the phone has one.
    pub stabilization: bool,
    /// Photos are HDR bursts, merged and tone mapped (the HDR chip).
    pub hdr: bool,
    /// The flash for photos; every launch starts at auto.
    #[serde(skip)]
    pub flash: FlashMode,
    /// Zooming past a lens's range switches to the next lens, and in dim
    /// light the main camera stands in for the telephoto. Off (the
    /// default), the lens changes only when the user picks another one.
    pub switch_lenses_by_zoom: bool,
    /// A tap shows an exposure slider beside the focus ring for a few
    /// seconds (off: exposure correction is in the manual controls).
    pub tap_ev_slider: bool,
    /// In landscape the shutter lies at the left edge of the screen, for
    /// the left hand (off, the default: at the right, either way the phone
    /// is held).
    pub shutter_left: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            aspect: Aspect::Full,
            resolution: Resolution::Full,
            jpeg_quality: DEFAULT_QUALITY,
            timer: Timer::Off,
            grid: Grid::Off,
            mirror_front: true,
            location: false,
            shutter_sound: true,
            save_dir: None,
            volume_shutter: true,
            tap_to_focus: true,
            debug_overlay: false,
            video_resolution: VideoResolution::FullHd,
            low_light: true,
            video_fps: 30,
            stabilization: true,
            hdr: false,
            flash: FlashMode::Auto,
            switch_lenses_by_zoom: false,
            tap_ev_slider: false,
            shutter_left: false,
        }
    }
}

impl Settings {
    /// `~/.config/omacam/settings.toml`.
    pub fn default_path() -> PathBuf {
        gtk4::glib::user_config_dir().join("omacam/settings.toml")
    }

    /// Load from `path`; defaults when it's missing. A file that doesn't
    /// parse is moved to `settings.toml.bad` and defaults are used.
    pub fn load(path: &Path) -> Settings {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Settings::default(),
        };
        match toml::from_str::<Settings>(&text) {
            Ok(s) => s.sanitized(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "settings unreadable; using defaults");
                let _ = std::fs::rename(path, path.with_extension("toml.bad"));
                Settings::default()
            }
        }
    }

    /// Write to `path` atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text =
            toml::to_string_pretty(&self.clone().sanitized()).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.part");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    fn sanitized(mut self) -> Settings {
        self.jpeg_quality = self.jpeg_quality.clamp(50, 100);
        self.video_fps = if self.video_fps >= 60 { 60 } else { 30 };
        if self
            .save_dir
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            self.save_dir = None;
        }
        self
    }

    /// The directory photos are saved in.
    pub fn photo_dir(&self) -> PathBuf {
        self.save_dir.clone().unwrap_or_else(default_photo_dir)
    }
}

/// Where videos go: `$XDG_VIDEOS_DIR/Camera`, beside the photos' folder
/// in Pictures, as desktop Linux keeps them apart.
pub fn default_video_dir() -> PathBuf {
    gtk4::glib::user_special_dir(gtk4::glib::UserDirectory::Videos)
        .unwrap_or_else(|| gtk4::glib::home_dir().join("Videos"))
        .join("Camera")
}

pub fn default_photo_dir() -> PathBuf {
    gtk4::glib::user_special_dir(gtk4::glib::UserDirectory::Pictures)
        .unwrap_or_else(|| gtk4::glib::home_dir().join("Pictures"))
        .join("Camera")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omacam-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.toml")
    }

    #[test]
    fn defaults_follow_the_owners_answers() {
        let s = Settings::default();
        assert!(s.shutter_sound, "Q7: shutter sound on");
        assert!(!s.location, "Q5: location off");
        assert!(s.mirror_front, "front photos saved as previewed");
        assert!(s.tap_to_focus);
        assert!(!s.debug_overlay);
        assert_eq!(s.aspect, Aspect::Full);
        assert_eq!(s.resolution, Resolution::Full);
        assert_eq!(s.jpeg_quality, 92);
        assert_eq!(s.timer, Timer::Off);
        assert_eq!(s.grid, Grid::Off);
        assert_eq!(s.save_dir, None);
        assert!(s.low_light, "the low-light lens on");
        assert_eq!(s.video_fps, 30);
        assert!(s.stabilization);
        assert_eq!(s.flash, FlashMode::Auto);
        assert!(!s.switch_lenses_by_zoom, "the lens changes only by hand");
        assert!(!s.shutter_left, "the shutter at the right in landscape");
    }

    #[test]
    fn a_missing_file_gives_defaults() {
        let p = tmp("missing");
        assert_eq!(Settings::load(&p), Settings::default());
    }

    #[test]
    fn save_and_load_round_trip() {
        let p = tmp("roundtrip");
        let s = Settings {
            aspect: Aspect::Square,
            resolution: Resolution::Medium,
            jpeg_quality: 80,
            timer: Timer::Ten,
            grid: Grid::Thirds,
            mirror_front: false,
            location: false,
            shutter_sound: false,
            save_dir: Some("/tmp/x".into()),
            volume_shutter: false,
            tap_to_focus: false,
            debug_overlay: true,
            video_resolution: VideoResolution::Hd,
            low_light: false,
            video_fps: 60,
            stabilization: false,
            hdr: true,
            flash: FlashMode::Auto,
            switch_lenses_by_zoom: true,
            tap_ev_slider: true,
            shutter_left: true,
        };
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        // The quick settings start afresh at every launch.
        assert!(
            !text.contains("aspect") && !text.contains("timer"),
            "{text}"
        );
        assert!(!text.contains("flash"), "{text}");
        assert!(text.contains("grid = \"3x3\""), "{text}");
        assert!(text.contains("video_resolution = \"720p\""), "{text}");
        assert_eq!(
            Settings::load(&p),
            Settings {
                aspect: Aspect::Full,
                timer: Timer::Off,
                flash: FlashMode::Auto,
                ..s
            }
        );
    }

    #[test]
    fn partial_and_unknown_keys_keep_defaults() {
        let p = tmp("partial");
        std::fs::write(&p, "shutter_sound = false\nsomething_new = 3\n").unwrap();
        let s = Settings::load(&p);
        assert!(!s.shutter_sound);
        assert_eq!(s.jpeg_quality, DEFAULT_QUALITY);
    }

    #[test]
    fn a_launch_starts_the_quick_settings_and_manual_values_afresh() {
        // As an older omacam wrote them: all ignored now.
        let p = tmp("afresh");
        std::fs::write(
            &p,
            "aspect = \"1:1\"\ntimer = \"10s\"\nflash = \"on\"\nkeep_pro = true\n\
             grid = \"3x3\"\n[pro]\niso = 200\n",
        )
        .unwrap();
        let s = Settings::load(&p);
        assert_eq!(
            (s.aspect, s.timer, s.flash),
            (Aspect::Full, Timer::Off, FlashMode::Auto)
        );
        assert_eq!(s.grid, Grid::Thirds, "the sheet's settings stay");
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let p = tmp("clamp");
        std::fs::write(
            &p,
            "jpeg_quality = 10\nsave_dir = \"\"\nvideo_fps = 45\nflash = \"torch\"\n",
        )
        .unwrap();
        let s = Settings::load(&p);
        assert_eq!(s.flash, FlashMode::Auto, "the flash starts at auto");
        assert_eq!(s.jpeg_quality, 50);
        assert_eq!(s.video_fps, 30, "only 30 or 60");
        assert_eq!(s.save_dir, None);
    }

    #[test]
    fn a_broken_file_is_moved_aside_not_lost() {
        let p = tmp("broken");
        std::fs::write(&p, "aspect = = nonsense").unwrap();
        assert_eq!(Settings::load(&p), Settings::default());
        assert!(p.with_extension("toml.bad").exists());
        assert!(!p.exists());
    }

    #[test]
    fn aspect_ratios_are_portrait() {
        assert_eq!(Aspect::FourThree.portrait_ratio(), Some(0.75));
        assert_eq!(Aspect::SixteenNine.portrait_ratio(), Some(9.0 / 16.0));
        assert_eq!(Aspect::Square.portrait_ratio(), Some(1.0));
        assert_eq!(Aspect::Full.portrait_ratio(), None);
    }
}
