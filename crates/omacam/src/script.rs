//! A tiny test script for driving the app on a phone without touching it:
//! `--script "wait 3; zoom 5; wait 2; shot /tmp/tele.png; quit"`.
//!
//! Steps: `wait SECONDS`, `zoom Z` (as a preset button), `pinch Z` (as a
//! pinch that ends at Z), `zoom-field` (a tap on the zoom field: the
//! dial opens), `mark Z` (a tap on the dial's mark Z, relative to the lens's
//! own view), `dial Z` (the zoom dial, dragged to Z), `camera main|macro|wide|tele`
//! (a lens from the lens menu), `lens-menu` (open it), `flip`, `macro`
//! (the macro lens from the menu, or back to the main camera),
//! `shutter`, `tap U V` (a tap on the picture), `settings PAGE` and
//! `close-settings`, `shot PATH` (the window as a PNG, rendered by the app
//! itself, so it works with the panel off), `hold
//! normal|left|right|upside-down` (as if the accelerometer said the phone
//! is held so), `mode night|photo|video`, `hdr on|off`, `manual on|off`
//! (the manual controls; `pro` is the same), `manual-edit
//! ev|focus|wb|shutter|iso|none` (open a value's slider), `focus P|auto`,
//! `iso N|auto`, `wb KELVIN|auto`, `ev EV`, `pro-shutter MICROSECONDS`,
//! `histogram on|off`, `lock`, `flash auto|on|off|torch`, `flash-menu`
//! (the flash's choices), `aspect full|4:3|16:9|1:1`, `aspect-menu`,
//! `timer 0|3|10`, and
//! `quit`.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Wait(Duration),
    Zoom(f64),
    Pinch(f64),
    Flip,
    /// The macro lens from the lens menu, or back to the main camera.
    Macro,
    /// A lens from the lens menu.
    Camera(omacam_core::lens::Role),
    /// Open the lens menu.
    LensMenu,
    /// Open the aspect ratios.
    AspectMenu,
    Shutter,
    Shot(PathBuf),
    /// Open the settings sheet on a page (photo, video, advanced).
    Settings(String),
    CloseSettings,
    /// A tap on the picture at (u, v), 0..1.
    Tap(f64, f64),
    /// Switch to night, photo or video (`shutter` then records and stops).
    Mode(String),
    /// The HDR chip.
    Hdr(bool),
    /// A tap on the zoom field (the dial opens).
    ZoomField,
    /// A tap on the dial's mark for this zoom, relative to the lens.
    Mark(f64),
    /// The zoom dial, as if dragged to this zoom.
    Dial(f64),
    /// Open or close the manual controls.
    Pro(bool),
    /// Open a value's slider: ev, shutter, iso, wb or focus (none closes).
    ProEdit(String),
    /// Manual focus at this lens position, or auto (None).
    Focus(Option<f64>),
    /// Manual ISO, or auto.
    Iso(Option<u32>),
    /// White balance at this color temperature, or auto.
    Wb(Option<u32>),
    Histogram(bool),
    /// Open the flash's choices.
    FlashMenu,
    /// The photo's aspect ratio: full, 4:3, 16:9 or 1:1.
    Aspect(String),
    /// The self-timer, seconds (0, 3 or 10).
    Timer(u32),
    /// Set a manual shutter time (µs; 0 for auto), as the Pro panel would.
    ProShutter(u32),
    /// AE/AF lock (a long press).
    Lock,
    /// The EV slider.
    Ev(f64),
    /// How the phone is held, as the accelerometer would say.
    Hold(String),
    /// The flash mode, as the flash button sets it.
    Flash(omacam_core::flash::FlashMode),
    Quit,
}

pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for part in text
        .split([';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let (cmd, arg) = part
            .split_once(char::is_whitespace)
            .map(|(c, a)| (c, a.trim()))
            .unwrap_or((part, ""));
        let number = || {
            arg.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| format!("{cmd}: needs a number, got {arg:?}"))
        };
        steps.push(match cmd {
            "wait" => Step::Wait(Duration::from_secs_f64(number()?)),
            "zoom" => Step::Zoom(number()?),
            "pinch" => Step::Pinch(number()?),
            "flip" => Step::Flip,
            "macro" => Step::Macro,
            "camera" => Step::Camera(match arg {
                "main" => omacam_core::lens::Role::Main,
                "macro" => omacam_core::lens::Role::Macro,
                "wide" => omacam_core::lens::Role::Ultrawide,
                "tele" => omacam_core::lens::Role::Tele,
                _ => return Err(format!("camera: main, macro, wide or tele, not {arg:?}")),
            }),
            "lens-menu" => Step::LensMenu,
            "aspect-menu" => Step::AspectMenu,
            "shutter" => Step::Shutter,
            "shot" if !arg.is_empty() => Step::Shot(PathBuf::from(arg)),
            "shot" => return Err("shot: needs a path".into()),
            "settings" => Step::Settings(if arg.is_empty() {
                "photo".into()
            } else {
                arg.into()
            }),
            "close-settings" => Step::CloseSettings,
            "mode" if matches!(arg, "night" | "photo" | "video") => Step::Mode(arg.into()),
            "hdr" if arg == "on" || arg == "off" => Step::Hdr(arg == "on"),
            "zoom-field" => Step::ZoomField,
            "mark" => Step::Mark(number()?),
            "dial" => Step::Dial(number()?),
            "pro" | "manual" if arg == "on" || arg == "off" => Step::Pro(arg == "on"),
            "pro-edit" | "manual-edit"
                if ["ev", "shutter", "iso", "wb", "focus", "none"].contains(&arg) =>
            {
                Step::ProEdit(arg.into())
            }
            "focus" if arg == "auto" => Step::Focus(None),
            "focus" => Step::Focus(Some(number()?)),
            "iso" if arg == "auto" => Step::Iso(None),
            "iso" => Step::Iso(Some(number()? as u32)),
            "wb" if arg == "auto" => Step::Wb(None),
            "wb" => Step::Wb(Some(number()? as u32)),
            "histogram" if arg == "on" || arg == "off" => Step::Histogram(arg == "on"),
            "flash-menu" => Step::FlashMenu,
            "aspect" if matches!(arg, "full" | "4:3" | "16:9" | "1:1") => Step::Aspect(arg.into()),
            "timer" if matches!(arg, "0" | "3" | "10") => Step::Timer(number()? as u32),
            "pro-shutter" => Step::ProShutter(number()? as u32),
            "lock" => Step::Lock,
            "ev" => Step::Ev(
                arg.parse::<f64>()
                    .map_err(|_| format!("ev: a number, got {arg:?}"))?,
            ),
            "mode" => return Err(format!("mode: night, photo or video, got {arg:?}")),
            "tap" => {
                let mut it = arg.split_whitespace().map(|v| v.parse::<f64>());
                match (it.next(), it.next()) {
                    (Some(Ok(u)), Some(Ok(v)))
                        if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) =>
                    {
                        Step::Tap(u, v)
                    }
                    _ => return Err(format!("tap: needs two numbers in 0..1, got {arg:?}")),
                }
            }
            "hold" if matches!(arg, "normal" | "left" | "right" | "upside-down") => {
                Step::Hold(arg.into())
            }
            "flash" => Step::Flash(arg.parse()?),
            "quit" => Step::Quit,
            other => return Err(format!("unknown step {other:?}")),
        });
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_script() {
        let s = parse("wait 1.5; zoom 5\npinch 0.6; flip; shutter; shot /tmp/a.png; quit").unwrap();
        assert_eq!(
            s,
            vec![
                Step::Wait(Duration::from_millis(1500)),
                Step::Zoom(5.0),
                Step::Pinch(0.6),
                Step::Flip,
                Step::Shutter,
                Step::Shot("/tmp/a.png".into()),
                Step::Quit,
            ]
        );
    }

    #[test]
    fn rejects_bad_steps() {
        assert!(parse("wait").is_err());
        assert!(parse("zoom x").is_err());
        assert!(parse("shot").is_err());
        assert!(parse("dance").is_err());
        assert!(parse("tap 2 0.5").is_err());
        assert_eq!(
            parse("mode video").unwrap(),
            vec![Step::Mode("video".into())]
        );
        assert!(parse("mode film").is_err());
        assert_eq!(
            parse("pro on; pro-edit iso; lock; ev -1.3; pro-shutter 4000").unwrap(),
            vec![
                Step::Pro(true),
                Step::ProEdit("iso".into()),
                Step::Lock,
                Step::Ev(-1.3),
                Step::ProShutter(4000)
            ]
        );
        assert_eq!(parse("tap 0.25 0.75").unwrap(), vec![Step::Tap(0.25, 0.75)]);
        assert_eq!(
            parse(
                "manual on; manual-edit focus; focus 0.3; iso auto; zoom-field; mark 2; dial 1.5"
            )
            .unwrap(),
            vec![
                Step::Pro(true),
                Step::ProEdit("focus".into()),
                Step::Focus(Some(0.3)),
                Step::Iso(None),
                Step::ZoomField,
                Step::Mark(2.0),
                Step::Dial(1.5),
            ]
        );
        assert_eq!(
            parse("flash torch; aspect 16:9; timer 3; wb 4300").unwrap(),
            vec![
                Step::Flash(omacam_core::flash::FlashMode::Torch),
                Step::Aspect("16:9".into()),
                Step::Timer(3),
                Step::Wb(Some(4300)),
            ]
        );
        assert!(parse("flash bright").is_err());
        assert!(parse("timer 5").is_err());
        assert_eq!(parse("hold left").unwrap(), vec![Step::Hold("left".into())]);
        assert!(parse("hold sideways").is_err());
        assert_eq!(
            parse("camera macro; lens-menu; aspect-menu").unwrap(),
            vec![
                Step::Camera(omacam_core::lens::Role::Macro),
                Step::LensMenu,
                Step::AspectMenu
            ]
        );
        assert!(parse("camera front").is_err());
        assert_eq!(
            parse("flash torch; flash auto").unwrap(),
            vec![
                Step::Flash(omacam_core::flash::FlashMode::Torch),
                Step::Flash(omacam_core::flash::FlashMode::Auto)
            ]
        );
        assert!(parse("flash").is_err());
        assert!(parse("flash strobe").is_err());
        assert_eq!(
            parse("settings video; close-settings").unwrap(),
            vec![Step::Settings("video".into()), Step::CloseSettings]
        );
        assert!(parse("wait -1").is_err());
        assert_eq!(parse(" ; ").unwrap(), vec![]);
    }
}
