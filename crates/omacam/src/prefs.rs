//! The settings sheet: a libadwaita preferences dialog opened from the gear
//! in the viewfinder's top bar. On a phone it opens as a bottom sheet.
//!
//! Every change is applied at once through `on_change` (which saves the
//! settings file). Settings that need features not built yet show as
//! disabled rows with the reason.
//!
//! No row holds a text field. On a phone a focused text field brings up
//! the on-screen keyboard, and libadwaita's text rows take the focus more
//! eagerly than a tap on the field: a spin row's text on a press of its
//! buttons or a swipe across it, an entry row's on any press on the row,
//! a scroll of the sheet included. JPEG quality is a list of values and
//! the photo folder comes from the folder chooser instead.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::settings::{
    default_photo_dir, default_video_dir, Aspect, Grid, Resolution, Settings, Timer,
    VideoResolution,
};

type OnChange = Rc<dyn Fn(&Settings)>;

#[allow(clippy::too_many_arguments)]
fn combo<T: Copy + PartialEq + 'static>(
    title: &str,
    subtitle: Option<&str>,
    all: &'static [T],
    label: fn(T) -> &'static str,
    current: T,
    state: &Rc<RefCell<Settings>>,
    on_change: &OnChange,
    set: fn(&mut Settings, T),
) -> adw::ComboRow {
    let labels: Vec<&str> = all.iter().map(|v| label(*v)).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .selected(all.iter().position(|v| *v == current).unwrap_or(0) as u32)
        .build();
    if let Some(s) = subtitle {
        row.set_subtitle(s);
    }
    let (state, on_change) = (state.clone(), on_change.clone());
    row.connect_selected_notify(move |r| {
        if let Some(v) = all.get(r.selected() as usize) {
            set(&mut state.borrow_mut(), *v);
            on_change(&state.borrow());
        }
    });
    row
}

fn switch(
    title: &str,
    subtitle: &str,
    active: bool,
    state: &Rc<RefCell<Settings>>,
    on_change: &OnChange,
    set: fn(&mut Settings, bool),
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(active)
        .build();
    let (state, on_change) = (state.clone(), on_change.clone());
    row.connect_active_notify(move |r| {
        set(&mut state.borrow_mut(), r.is_active());
        on_change(&state.borrow());
    });
    row
}

/// A row for something not built yet.
fn later(title: &str, reason: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(reason)
        .sensitive(false)
        .build()
}

fn info(title: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .subtitle_selectable(true)
        .build()
}

/// The JPEG qualities offered, and the one in the settings if it is
/// another (the file may hold any value from 50 to 100).
fn qualities(current: u8) -> Vec<u8> {
    let mut all = vec![50, 60, 70, 75, 80, 85, 90, 92, 95, 98, 100];
    if let Err(i) = all.binary_search(&current) {
        all.insert(i, current);
    }
    all
}

/// JPEG quality: a list, not a spin row (no text field, see the top).
fn quality_row(current: u8, state: &Rc<RefCell<Settings>>, on_change: &OnChange) -> adw::ComboRow {
    let all = qualities(current);
    let labels: Vec<String> = all.iter().map(|q| q.to_string()).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title("JPEG quality")
        .subtitle("Higher keeps more detail in bigger files")
        .model(&gtk::StringList::new(&labels))
        .selected(all.iter().position(|q| *q == current).unwrap_or(0) as u32)
        .build();
    let (state, on_change) = (state.clone(), on_change.clone());
    row.connect_selected_notify(move |r| {
        if let Some(q) = all.get(r.selected() as usize) {
            state.borrow_mut().jpeg_quality = *q;
            on_change(&state.borrow());
        }
    });
    row
}

/// Where photos are saved: the folder chooser, not an entry row (no text
/// field, see the top). The Pictures folder's Camera directory is the
/// default; choosing it again goes back to the default.
fn folder_row(
    current: &Path,
    state: &Rc<RefCell<Settings>>,
    on_change: &OnChange,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title("Save photos in")
        .subtitle(current.display().to_string())
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
    let (state, on_change) = (state.clone(), on_change.clone());
    row.connect_activated(move |row| {
        let chooser = gtk::FileDialog::builder()
            .title("Save photos in")
            .modal(true)
            .build();
        let dir = state.borrow().photo_dir();
        if dir.is_dir() {
            chooser.set_initial_folder(Some(&gtk::gio::File::for_path(&dir)));
        }
        let parent = row.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let (row, state, on_change) = (row.clone(), state.clone(), on_change.clone());
        chooser.select_folder(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            // An error is mostly the chooser dismissed: nothing changes.
            let Some(path) = res.ok().and_then(|f| f.path()) else {
                return;
            };
            row.set_subtitle(&path.display().to_string());
            state.borrow_mut().save_dir = (path != default_photo_dir()).then_some(path);
            on_change(&state.borrow());
        });
    });
    row
}

pub fn dialog(
    settings: &Settings,
    on_change: impl Fn(&Settings) + 'static,
) -> adw::PreferencesDialog {
    let state = Rc::new(RefCell::new(settings.clone()));
    let on_change: OnChange = Rc::new(on_change);
    let s = settings;

    // Photo.
    let photo = adw::PreferencesPage::builder()
        .title("Photo")
        .name("photo")
        .icon_name("camera-photo-symbolic")
        .build();
    let shape = adw::PreferencesGroup::builder().title("Picture").build();
    shape.add(&combo(
        "Aspect ratio",
        Some("Cropped from the center of the frame"),
        &Aspect::ALL,
        Aspect::label,
        s.aspect,
        &state,
        &on_change,
        |s, v| s.aspect = v,
    ));
    shape.add(&combo(
        "Resolution",
        Some("Full is each lens's largest mode"),
        &Resolution::ALL,
        Resolution::label,
        s.resolution,
        &state,
        &on_change,
        |s, v| s.resolution = v,
    ));
    shape.add(&quality_row(s.jpeg_quality, &state, &on_change));
    photo.add(&shape);

    let taking = adw::PreferencesGroup::builder()
        .title("Taking photos")
        .build();
    taking.add(&combo(
        "Timer",
        None,
        &Timer::ALL,
        Timer::label,
        s.timer,
        &state,
        &on_change,
        |s, v| s.timer = v,
    ));
    taking.add(&combo(
        "Grid",
        None,
        &Grid::ALL,
        Grid::label,
        s.grid,
        &state,
        &on_change,
        |s, v| s.grid = v,
    ));
    taking.add(&later("Level", "Coming later"));
    taking.add(&switch(
        "Shutter sound",
        "Play a sound when a photo is taken",
        s.shutter_sound,
        &state,
        &on_change,
        |s, v| s.shutter_sound = v,
    ));
    taking.add(&switch(
        "Save selfies as previewed",
        "Mirror front camera photos, as the preview shows them",
        s.mirror_front,
        &state,
        &on_change,
        |s, v| s.mirror_front = v,
    ));
    photo.add(&taking);

    let saving = adw::PreferencesGroup::builder().title("Saving").build();
    saving.add(&switch(
        "Location tags",
        "Record where photos and videos are taken (GeoClue)",
        s.location,
        &state,
        &on_change,
        |s, v| s.location = v,
    ));
    saving.add(&folder_row(&s.photo_dir(), &state, &on_change));
    photo.add(&saving);

    // Video.
    let video = adw::PreferencesPage::builder()
        .title("Video")
        .name("video")
        .icon_name("camera-video-symbolic")
        .build();
    let vgroup = adw::PreferencesGroup::builder()
        .title("Video")
        .description(format!(
            "Videos are saved in {} as MP4: H.264 video, Opus sound.",
            default_video_dir().display()
        ))
        .build();
    vgroup.add(&combo(
        "Resolution",
        Some("Landscape frames, turned upright by the player"),
        &VideoResolution::ALL,
        VideoResolution::label,
        s.video_resolution,
        &state,
        &on_change,
        |s, v| s.video_resolution = v,
    ));
    vgroup.add(&combo(
        "Frame rate",
        Some("60 fps where the lens's mode allows it, else 30"),
        &[30u32, 60],
        |f| if f >= 60 { "60 fps" } else { "30 fps" },
        s.video_fps,
        &state,
        &on_change,
        |s, v| s.video_fps = v,
    ));
    vgroup.add(&switch(
        "Stabilization",
        "Steadies videos with the gyroscope; crops 10 % more",
        s.stabilization,
        &state,
        &on_change,
        |s, v| s.stabilization = v,
    ));
    video.add(&vgroup);

    // Advanced and about.
    let advanced = adw::PreferencesPage::builder()
        .title("Advanced")
        .name("advanced")
        .icon_name("preferences-other-symbolic")
        .build();
    let adv = adw::PreferencesGroup::builder().title("Advanced").build();
    adv.add(&switch(
        "Tap to focus",
        "Tap the picture to focus there",
        s.tap_to_focus,
        &state,
        &on_change,
        |s, v| s.tap_to_focus = v,
    ));
    adv.add(&switch(
        "Exposure slider at the focus point",
        "A tap also shows an exposure slider beside the focus ring",
        s.tap_ev_slider,
        &state,
        &on_change,
        |s, v| s.tap_ev_slider = v,
    ));
    adv.add(&switch(
        "Volume keys take photos",
        "While the camera is in front",
        s.volume_shutter,
        &state,
        &on_change,
        |s, v| s.volume_shutter = v,
    ));
    adv.add(&switch(
        "Keep manual settings",
        "Off: every launch starts fully automatic",
        s.keep_pro,
        &state,
        &on_change,
        |s, v| s.keep_pro = v,
    ));
    adv.add(&switch(
        "Switch lenses while zooming",
        "Off: the lens changes only when you pick another one",
        s.switch_lenses_by_zoom,
        &state,
        &on_change,
        |s, v| s.switch_lenses_by_zoom = v,
    ));
    adv.add(&switch(
        "Low-light lens",
        "With lenses switching while zooming: in dim light, the main camera \
         shows the telephoto's zoom (not with manual controls open)",
        s.low_light,
        &state,
        &on_change,
        |s, v| s.low_light = v,
    ));
    adv.add(&later("RAW (DNG) with JPEG", "Coming later"));
    adv.add(&switch(
        "Debug overlay",
        "Frame rate, lens and switch time over the picture",
        s.debug_overlay,
        &state,
        &on_change,
        |s, v| s.debug_overlay = v,
    ));
    advanced.add(&adv);

    let about = adw::PreferencesGroup::builder().title("About").build();
    about.add(&info(
        "Camera",
        &format!("omacam {}", env!("CARGO_PKG_VERSION")),
    ));
    about.add(&info("License", "GPL-3.0-or-later"));
    about.add(&info(
        "Credits",
        "Theme support adapted from gliff by Kevin McConnell (MIT)",
    ));
    advanced.add(&about);

    let dialog = adw::PreferencesDialog::builder()
        .title("Camera settings")
        .search_enabled(false)
        .build();
    dialog.add(&photo);
    dialog.add(&video);
    dialog.add(&advanced);
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualities_offer_the_one_in_the_settings() {
        let q = qualities(92);
        assert_eq!(q.iter().filter(|&&v| v == 92).count(), 1);
        assert_eq!((q[0], q[q.len() - 1]), (50, 100));
        let q = qualities(87);
        assert!(q.contains(&87));
        assert!(q.windows(2).all(|w| w[0] < w[1]));
    }
}
