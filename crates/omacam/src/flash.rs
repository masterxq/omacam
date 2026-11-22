//! The flash LED on the phone: found under `/sys/class/leds` and lit
//! through logind. The app sets a brightness only when the light it wants
//! changes: when the kernel dims the LED or puts it out on its own (it
//! guards the LED's temperature), the app leaves it so.
//!
//! The LED class files belong to root. logind lets the user of the active
//! graphical session set an LED's brightness
//! (`org.freedesktop.login1.Session.SetBrightness("leds", NAME, VALUE)`),
//! which is how Phosh's torch does it, so the app needs no rights of its
//! own. The session is the user's display session
//! (`org.freedesktop.login1.User.Display`), not the caller's: an app
//! started from the launcher runs in a systemd user scope outside any
//! session, and one started over ssh in a session without a seat, which
//! logind refuses.
//!
//! A helper process does the calls: the app starts `omacam --led-helper
//! NAME` with a pipe on its standard input and writes each brightness as a
//! line. When the app ends in any way, a kill included, the pipe closes
//! and the helper puts the LED out before it exits, so a crash can't leave
//! the torch on. The helper ignores SIGINT, SIGTERM and SIGHUP, which a
//! terminal or a session stop sends the whole process group, and waits for
//! the pipe instead.

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use omacam_core::flash::{FlashSpec, Led, Light};

const LEDS: &str = "/sys/class/leds";

/// The flash LED the app drives.
pub struct FlashLed {
    name: String,
    max: u32,
    torch: u32,
    photo: u32,
    helper: RefCell<Option<(Child, ChildStdin)>>,
    /// The brightness last asked for.
    level: std::cell::Cell<u32>,
}

impl FlashLed {
    /// The LED the lens table names, or one whose name says flash or
    /// torch; none when there is none or the table says so.
    pub fn find(spec: &FlashSpec) -> Option<FlashLed> {
        let name = match &spec.led {
            Led::None => return None,
            Led::Named(n) => n.clone(),
            Led::Find => find_led(Path::new(LEDS))?,
        };
        let dir = Path::new(LEDS).join(&name);
        let max = read_u32(&dir.join("max_brightness")).filter(|&m| m > 0);
        let Some(max) = max else {
            tracing::info!(led = name, "no flash: the LED isn't there");
            return None;
        };
        let level = |v: Option<u32>, default: u32| v.unwrap_or(default).clamp(1, max);
        let led = FlashLed {
            torch: level(spec.torch, max),
            photo: level(spec.photo, max),
            max,
            helper: RefCell::new(None),
            level: std::cell::Cell::new(0),
            name,
        };
        tracing::info!(
            led = led.name,
            max = led.max,
            torch = led.torch,
            photo = led.photo,
            "flash"
        );
        Some(led)
    }

    /// The LED's brightness for a light.
    pub fn level(&self, light: Light) -> u32 {
        match light {
            Light::Off => 0,
            Light::Torch => self.torch,
            Light::Photo => self.photo,
        }
    }

    /// Light the LED so, or put it out.
    pub fn set(&self, light: Light) {
        let level = self.level(light);
        self.level.set(level);
        if self.send(level).is_err() {
            // The helper is gone (or never started): once more with a new
            // one, else from here.
            self.helper.borrow_mut().take();
            if self.send(level).is_err() {
                self.helper.borrow_mut().take();
                if let Err(e) = Logind::connect().and_then(|l| l.set(&self.name, level)) {
                    tracing::warn!(led = self.name, level, "flash: {e}");
                }
            }
        }
    }

    fn send(&self, level: u32) -> std::io::Result<()> {
        let mut helper = self.helper.borrow_mut();
        if helper.is_none() {
            let mut child = Command::new(std::env::current_exe()?)
                .arg("--led-helper")
                .arg(&self.name)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()?;
            let stdin = child.stdin.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
            *helper = Some((child, stdin));
        }
        let (child, stdin) = helper.as_mut().expect("just set");
        if child.try_wait()?.is_some() {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        writeln!(stdin, "{level}")?;
        stdin.flush()
    }
}

impl Drop for FlashLed {
    fn drop(&mut self) {
        if self.level.get() != 0 {
            self.set(Light::Off);
        }
        // Closing the pipe ends the helper.
        if let Some((mut child, stdin)) = self.helper.borrow_mut().take() {
            drop(stdin);
            let _ = child.wait();
        }
    }
}

fn read_u32(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// An LED whose name's function part says flash or torch (`white:flash`,
/// `white:flash-1`, `:torch`), as Phosh and feedbackd look for them.
fn find_led(dir: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            let function = n.rsplit(':').next().unwrap_or("");
            let function = function.split('-').next().unwrap_or("");
            n.contains(':') && (function == "flash" || function == "torch")
        })
        .collect();
    names.sort();
    names.into_iter().next()
}

/// logind on the system bus.
struct Logind {
    bus: gio::DBusConnection,
}

const LOGIN1: &str = "org.freedesktop.login1";

impl Logind {
    fn connect() -> Result<Logind, String> {
        gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE)
            .map(|bus| Logind { bus })
            .map_err(|e| format!("system bus: {e}"))
    }

    /// The user's graphical session: the one the shell runs in.
    fn display_session(&self) -> Option<String> {
        let reply = self
            .bus
            .call_sync(
                Some(LOGIN1),
                "/org/freedesktop/login1/user/self",
                "org.freedesktop.DBus.Properties",
                "Get",
                Some(&("org.freedesktop.login1.User", "Display").to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
            )
            .ok()?;
        let (value,): (glib::Variant,) = reply.get()?;
        let (_, path): (String, glib::variant::ObjectPath) = value.as_variant()?.get()?;
        let path = path.as_str().to_string();
        (path != "/").then_some(path)
    }

    fn set_on(&self, session: &str, led: &str, level: u32) -> Result<(), glib::Error> {
        self.bus
            .call_sync(
                Some(LOGIN1),
                session,
                "org.freedesktop.login1.Session",
                "SetBrightness",
                Some(&("leds", led, level).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
            )
            .map(|_| ())
    }

    /// Set the LED's brightness through the user's display session, or
    /// the caller's own when there is none.
    fn set(&self, led: &str, level: u32) -> Result<(), String> {
        let auto = "/org/freedesktop/login1/session/auto";
        let session = self.display_session();
        let first = self.set_on(session.as_deref().unwrap_or(auto), led, level);
        match first {
            Ok(()) => Ok(()),
            Err(e) if session.is_some() => self
                .set_on(auto, led, level)
                .map_err(|e2| format!("SetBrightness: {e}; on this session: {e2}")),
            Err(e) => Err(format!("SetBrightness: {e}")),
        }
    }
}

/// `omacam --led-helper NAME`: set the LED to each brightness read from
/// standard input; at its end, put it out if it is lit.
pub fn helper(name: &str) -> glib::ExitCode {
    // SAFETY: setting signal dispositions before any thread starts.
    unsafe {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(sig, libc::SIG_IGN);
        }
    }
    let logind = match Logind::connect() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("flash helper: {e}");
            return glib::ExitCode::FAILURE;
        }
    };
    let mut lit = false;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(level) = line.trim().parse::<u32>() else {
            continue;
        };
        match logind.set(name, level) {
            Ok(()) => lit = level > 0,
            Err(e) => tracing::warn!(led = name, level, "flash: {e}"),
        }
    }
    if lit {
        if let Err(e) = logind.set(name, 0) {
            tracing::warn!(led = name, "flash: putting it out: {e}");
        } else {
            tracing::info!(led = name, "flash: out as the app ended");
        }
    }
    glib::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("omacam-flash-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_a_flash_or_torch_led_by_its_name() {
        let dir = tmp("leds");
        for n in ["white:status", "mmc0::", "white:flash-1", "red:indicator"] {
            std::fs::create_dir(dir.join(n)).unwrap();
        }
        assert_eq!(find_led(&dir).as_deref(), Some("white:flash-1"));
        std::fs::remove_dir(dir.join("white:flash-1")).unwrap();
        assert_eq!(find_led(&dir), None);
        std::fs::create_dir(dir.join(":torch")).unwrap();
        assert_eq!(find_led(&dir).as_deref(), Some(":torch"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
