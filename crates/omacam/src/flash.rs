//! The flash: the mode the controls choose, and the LED that carries it out.
//!
//! The controls own [`FlashMode`] (a setting, chosen in the top bar) and
//! tell a [`FlashLed`] what to do: the mode as it changes, and each photo
//! before and after it is taken. How the LED is driven lives behind the
//! trait; until a driver is wired in, [`Placeholder`] only logs, and the
//! controls work the same either way.

use serde::{Deserialize, Serialize};

/// How the flash is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlashMode {
    /// Fires for a photo when the scene is dark.
    Auto,
    /// Fires for every photo.
    On,
    #[default]
    Off,
    /// Lit all the time (a torch, for the macro lens as a magnifier, and
    /// for videos).
    Torch,
}

impl FlashMode {
    pub const ALL: [FlashMode; 4] = [
        FlashMode::Auto,
        FlashMode::On,
        FlashMode::Off,
        FlashMode::Torch,
    ];

    pub fn icon(self) -> &'static str {
        match self {
            FlashMode::Auto => "omacam-flash-auto-symbolic",
            FlashMode::On => "omacam-flash-on-symbolic",
            FlashMode::Off => "omacam-flash-off-symbolic",
            FlashMode::Torch => "omacam-torch-symbolic",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FlashMode::Auto => "Auto",
            FlashMode::On => "On",
            FlashMode::Off => "Off",
            FlashMode::Torch => "Torch",
        }
    }

    /// Videos have no photo to flash for: only the torch, or nothing.
    pub fn for_video(self) -> FlashMode {
        match self {
            FlashMode::Torch => FlashMode::Torch,
            _ => FlashMode::Off,
        }
    }

    /// Whether a photo taken now fires the flash; `dark` says whether the
    /// scene is dark (for Auto).
    pub fn fires(self, dark: bool) -> bool {
        match self {
            FlashMode::On => true,
            FlashMode::Auto => dark,
            FlashMode::Off | FlashMode::Torch => false,
        }
    }
}

/// The flash LED, as the controls see it. Calls come on the UI thread and
/// must not block it.
pub trait FlashLed {
    /// Whether the phone has a flash the app can drive (the flash
    /// controls hide otherwise).
    fn available(&self) -> bool;
    /// The mode changed: light the torch for [`FlashMode::Torch`], turn it
    /// off for any other mode.
    fn set_mode(&self, mode: FlashMode);
    /// A photo is about to be taken with the flash: light it (a pre-flash
    /// for the exposure, then the flash) until [`FlashLed::photo_done`].
    fn photo_starts(&self);
    /// The photo's frame is in: back to the mode's state.
    fn photo_done(&self);
}

/// Stands in for the flash until a driver is wired in: logs what it would
/// do, so the controls can be tried.
#[derive(Debug, Default)]
pub struct Placeholder;

impl FlashLed for Placeholder {
    fn available(&self) -> bool {
        true
    }

    fn set_mode(&self, mode: FlashMode) {
        tracing::info!(?mode, "flash (placeholder): mode");
    }

    fn photo_starts(&self) {
        tracing::info!("flash (placeholder): fires for the photo");
    }

    fn photo_done(&self) {
        tracing::info!("flash (placeholder): photo done");
    }
}

/// A scene the auto flash fires for: the exposure the camera chose (time
/// times total gain) past 1/15 s at gain 4, about ISO 400 at 1/15 s.
pub fn is_dark(exposure_us: Option<i32>, gain: Option<f32>) -> bool {
    match (exposure_us, gain) {
        (Some(t), Some(g)) => t as f64 * g as f64 >= 66_667.0 * 4.0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_fire_as_named() {
        assert!(FlashMode::On.fires(false));
        assert!(FlashMode::Auto.fires(true) && !FlashMode::Auto.fires(false));
        assert!(!FlashMode::Off.fires(true) && !FlashMode::Torch.fires(true));
        assert_eq!(FlashMode::On.for_video(), FlashMode::Off);
        assert_eq!(FlashMode::Torch.for_video(), FlashMode::Torch);
    }

    #[test]
    fn dark_is_a_long_exposure_at_high_gain() {
        assert!(!is_dark(Some(10_000), Some(1.0)));
        assert!(is_dark(Some(66_667), Some(4.0)));
        assert!(!is_dark(None, Some(16.0)));
    }
}
