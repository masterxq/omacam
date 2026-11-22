# omacam

omacam is the camera app of Omarchy Mobile, shown as **Camera**. It is made
for phones: one zoom control runs from the ultrawide through the main camera
to the telephoto and picks the lens for you, as the stock Android camera
does, and photos come from the camera's full-resolution mode instead of the
preview.

It talks to the cameras through [libcamera](https://libcamera.org) directly,
not through PipeWire, so it can use what a webcam pipeline can't: every
camera of the phone, its modes and its controls. The window follows your
Omarchy theme.

The reference phone is the Google Pixel 7 Pro running
[Omarchy Mobile](https://github.com/derekross/omarchy-mobile). On other
machines omacam uses the first back camera and the first front camera it
finds.

## What you get

- A viewfinder that takes the screen's whole width, upright and mirrored
  for selfies, with the controls over it where it runs under them; it
  never shows less than the photo gets. Held sideways, the layout stays
  and the icons turn.
- Quick settings along the top: flash (auto, on, off, torch; a long
  press switches the torch), self-timer, aspect ratio (its choices drop
  down), HDR or 60 fps, the settings. Every launch starts them afresh:
  flash on auto, no timer, the full frame, the main camera at 1x, all
  manual values automatic.
- Zoom and lenses apart: the zoom field shows the zoom of the lens in use
  ("1.0×"); tap it (or drag across it) for the zoom dial, whose marks at
  the lens's least and most zoom and at 1× go there with a tap, or pinch
  the picture (it rests a moment at 1x). Each lens zooms as far as the
  lens table says. Zooming never changes the camera. The lens button beside the stages
  shows the lens in use; tap it for the lenses, each with its symbol and
  name (Wide, Main, Tele, Macro), and the chosen lens's name shows over
  the picture for a moment. (The setting
  "Switch lenses while zooming" brings back the ultrawide, main and
  telephoto taking over as you pass 1x and 4.78x, with a short
  cross-fade.) (Until the main camera has a 4:3 preview mode,
  its 16:9 preview fills the screen and shows less than its photos: the
  photo holds about a third more width than the preview at 1x, except in
  16:9, and the view jumps by as much at its switches.)
- Low light, with lenses switching while zooming: when it's too dark for
  the telephoto, its zoom comes from the main camera instead (a "Low
  light" note shows), and the telephoto comes back when the light does.
  Not with the manual controls open.
- Photos from the full-resolution mode (12.5 MP from the main camera),
  cropped to the zoom and saved upright as JPEG in `~/Pictures/Camera`.
- Switching between the back and the front camera.
- Continuous autofocus. Tap the picture to focus and meter there (in
  video, autofocus keeps following the tapped spot); four thin corners
  mark the spot, light with a dark edge so they show on any picture,
  and stay small and faint while focus and metering follow it. A tap on
  them (or leaving the app) gives the whole picture back. Long-press for
  AE/AF lock.
- Manual controls behind one button beside the lens row: exposure
  correction, focus, white balance, shutter time and ISO, and a
  histogram. Everything stays automatic until you move a value's slider;
  values set by hand show in the accent color with a dot, and "Auto" (or
  a long press on the value) gives one back. Each control appears as the
  camera offers it.
- The last photo as a thumbnail; tap it to open it.
- Video: 1080p or 720p at 30 fps, or 60 where the lens's mode allows it,
  H.264 with Opus sound in MP4, saved in `~/Videos/Camera`. Zoom while
  recording switches lenses as it does for photos, without a break in the
  file. Stabilization from the gyroscope, where the phone has one.
- Photos with EXIF data (time to the millisecond, exposure, ISO, aperture,
  focal length, which camera, zoom; no serial numbers) and the shutter
  sound. Location tags in photos and videos, from GeoClue, when you turn
  them on (off by default). Photos and videos follow how the phone is
  held, and so do the controls: held sideways, they turn in place.
- Settings from the gear: resolution, JPEG quality, a 3x3 or 4x4 grid, mirrored selfies,
  the shutter sound, location tags, where photos are saved, the video
  frame rate and stabilization, volume keys as the shutter, tap to focus,
  the low-light lens, and the debug overlay. They are kept in
  `~/.config/omacam/settings.toml`.
- Night mode and an HDR chip (Photo mode): a burst at one locked exposure
  (Night 12 frames, HDR 8), aligned and merged on the phone into one photo.
  The burst's raw sensor frames are merged before the color processing,
  which keeps the shadows and cuts the noise further (`--no-raw-merge`
  merges the processed frames instead). Hold still while it's taken; the
  merge takes a few seconds. Where the camera merges a series of frames of
  its own for each photo (the Moto G60s), both stay hidden: the lens
  table's `bursts = false`.
- The cameras are let go while the window is hidden, so other apps (a
  browser, a video call) can use them.

Not yet: RAW, a level, and a stabilized preview
(the file is stabilized; the viewfinder shows its framing). See
[docs/architecture.md](docs/architecture.md) for the plan.

## Install

Once omacam is in Omarchy Mobile's package repository, install the
`omacam` package. To build and install the package from a checkout on the
phone itself:

```
bin/install
```

To build it on an x86_64 laptop for the phone, see
[docs/development.md](docs/development.md) (`bin/cross package`).

## Use

Open **Camera** from the app launcher, or run `omacam`.

```
omacam --list            # the cameras and the role each one plays
omacam --front           # open on the front camera
omacam --zoom 2          # open at 2x
omacam --capture quick   # photos from the preview, without the mode switch
omacam --remote shutter  # take a photo with the running omacam
```

Volume keys take photos when they reach the window. On Omarchy Mobile the
shell binds them for the system volume, so it has to hand them over with
`omacam --remote shutter` while the camera is in front.

`--display dmabuf` shows the camera's buffers without copying them, which
saves CPU and battery; it needs a Mesa with the panthor export fix, and
becomes the default once that ships. `--debug` shows the frame rate, the
lens and the lens switch time over the picture.

A lens table describes the cameras of a phone: which is which, how much
each one zooms, and which modes to use. omacam has the Pixel 7 Pro's built
in (`data/cheetah.toml`); `~/.config/omacam/lenses.toml` or
`--lens-table FILE` replaces it.

## Picture quality

omacam uses libcamera's software ISP. There is no HDR+ and no Google
tuning, so pictures are not as good as the stock camera's. The telephoto's
focus motor doesn't work yet on the Pixel 7 Pro, so its pictures are soft.

## More

- [docs/architecture.md](docs/architecture.md): how it works and why.
- [docs/development.md](docs/development.md): building, cross-compiling for
  the phone, testing.

omacam is released under the GNU General Public License, version 3 or
later. Its theme support is adapted from
[gliff](https://github.com/omacom/gliff) (MIT); see NOTICE.
