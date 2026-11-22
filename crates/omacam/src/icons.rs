//! omacam's own symbolic icons (`data/icons`), built into the binary.
//!
//! GTK finds icons by name in the icon theme's search path, and recolors
//! a file whose name ends in `-symbolic.svg` to the widget's color. The
//! source tree's `data/icons` is used where it exists (a build run from
//! its checkout); otherwise the built-in copies are written to
//! `$XDG_RUNTIME_DIR/omacam/icons` once per start, so a lone binary
//! copied to a phone has its icons too.

use gtk::gdk;
use gtk4 as gtk;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$((
            concat!("omacam-", $name, "-symbolic.svg"),
            include_str!(concat!("../../../data/icons/omacam-", $name, "-symbolic.svg")),
        )),*]
    };
}

/// File name and contents of every icon.
const ICONS: &[(&str, &str)] = icons!(
    "adjust",
    "exposure",
    "far",
    "flash-auto",
    "flash-off",
    "flash-on",
    "flip",
    "gallery",
    "histogram",
    "lens-main",
    "lens-tele",
    "lens-wide",
    "macro",
    "night",
    "settings",
    "timer",
    "torch",
);

/// Make the icons findable by name (`omacam-flash-on-symbolic`).
pub fn register(display: &gdk::Display) {
    let theme = gtk::IconTheme::for_display(display);
    let source = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/icons"));
    if source.join(ICONS[0].0).exists() {
        theme.add_search_path(source);
        return;
    }
    let dir = gtk::glib::user_runtime_dir().join("omacam/icons");
    let written = std::fs::create_dir_all(&dir).and_then(|()| {
        for (name, svg) in ICONS {
            std::fs::write(dir.join(name), svg)?;
        }
        Ok(())
    });
    match written {
        Ok(()) => theme.add_search_path(&dir),
        Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "icons not written"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_16px_symbolic_svg_with_its_license() {
        for (name, svg) in ICONS {
            assert!(name.ends_with("-symbolic.svg"), "{name}");
            assert!(svg.contains("SPDX-License-Identifier: GPL-3.0-or-later"));
            assert!(svg.contains(r#"viewBox="0 0 16 16""#), "{name}");
            assert!(!svg.contains("stroke"), "{name}: fills only");
        }
    }
}
