# omacam architecture and design

omacam is a phone camera on libcamera, GTK 4 and libadwaita. This document
covers what is built, the design choices behind it, and what is pending.
The design follows Omarchy Mobile's camera app plan
(`docs/CAMERA-APP-PLAN.md` in omarchy-mobile); section numbers below refer
to it. Building and testing are in `docs/development.md`.

## Crates

```
omacam-core        no UI, no camera library; unit-tested
  backend          the Backend trait, commands, events, frames
  lens             the lens table (TOML) matched to the cameras present
  zoom             the lens ladder, thresholds, hysteresis, slider maths
  switch           the stream state machine (stop one camera, start the next)
  capture          the still sequence (lock, reconfigure, discard, keep, restore;
                   or one frame of a still stream beside the preview)
  pixels           reading RGB, YUYV and NV12 frames as RGB
  exposure         carrying exposure across a lens switch by aperture
  exposure_control the manual values, metering, AE/AF lock
  focus            tap to focus: picture point to sensor window, the scan
  framing          which part of each lens's frame a zoom shows
  lowlight         the telephoto's low-light fallback to the main camera
  stabilize        video stabilization from the gyroscope
  orientation      how the phone is held
  controller       ties them together: input and events in, commands out
omacam-libcamera   the backend on libcamera-rs, on its own thread
omacam-record      the video recorder (GStreamer): frames and sound into MP4
omacam-fake        a backend with synthetic frames, for tests and the laptop
omacam             the GTK app: viewfinder, controls, JPEG writing, theme,
                   sensors (accelerometer, gyroscope), GeoClue
```

## Data flow

```
  UI thread (GTK)                                  backend thread
  ───────────────                                  ──────────────
  gestures, buttons ─► Controller ─► Command ───►  CameraManager, ActiveCamera
                          ▲                         requests, buffers
                          │ Event                       │
  Viewfinder ◄─ Show ─────┤ ◄── channel + frame slot ◄──┘ completed request:
  (GdkMemoryTexture       │                              copy (memory) or lend
   or GdkDmabufTexture)   └─ Picture ─► encoder thread ─► JPEG in ~/Pictures/Camera
```

libcamera-rs's `ActiveCamera` isn't `Send`, so the camera manager, the
camera and its buffers live on one backend thread. Commands, completed
requests and returned buffers reach it through one channel and are handled
in order. Every stream start has a generation number; frames and events
carry it, and the controller drops anything from a stream it has left.

Frames reach the UI through a one-frame slot: a new frame replaces an
unshown one, whose buffer goes back to the camera at once, so a slow UI
skips frames instead of queueing 9 MB copies.

## The cameras on gs201

- **One camera streams at a time** (all CSIS links share one write DMA
  context), so a lens switch is a stop and a start. The switch state
  machine keeps at most one start in flight; requests that arrive while a
  camera starts only replace the target, so a pinch through three lenses
  costs at most two starts. Measured on the Pixel 7 Pro, from the request to
  the first frame of the new camera: 280-420 ms (main, ultrawide, front),
  about 620 ms to the telephoto.
- **One processed stream.** libcamera's software ISP gives one processed
  stream, so a full-resolution still is a reconfiguration into the large
  mode (§3.3): wait for autofocus (at most 500 ms), note exposure and gains,
  start the still mode, drop at least two frames and at most six until the
  exposure is back, keep one, and start the preview again. The main camera's
  still mode is 4072x3072 at about 9 fps, the ultrawide's 4032x3016. A
  still whose mode is the preview's (on a camera whose table gives no
  larger one) is the next preview frame, with no stop and start.
- **A still stream beside the preview**, where the camera has one. A photo
  preview asks libcamera for the viewfinder and still roles in one
  configuration (the still at the lens table's still size). Where it
  validates the pair (two processed streams, the still larger than the
  preview, formats omacam reads), the backend says so in `Started`, and a
  photo is one still buffer added to the next request (`Command::Capture`):
  no stop and start, the preview runs on, and the exposure and focus are
  the preview's, settled. The shutter waits only while autofocus scans or
  the exposure is still searching (at most 500 ms). The still frame
  reaches the controller past the viewfinder's one-frame slot. A camera
  that refuses the pair (libcamera's simple pipeline gives one processed
  stream) starts the preview alone and isn't asked again; its photos
  reconfigure as above. Bursts (Night, HDR) always reconfigure.
  On the Moto G60s (mtkisp50, the MediaTek DIP's two outputs) a photo is
  in its buffer 150-165 ms after the shutter instead of about 2 s.
- **Camera memory.** Every start frees the previous stream's buffers
  first, and a still asks for two buffers. A start that fails for lack of
  memory is retried once after releasing the camera manager, then with one
  buffer, then at the preview size; the user is told when a photo came out
  smaller. Each failure is logged with the step that failed (acquire,
  configure, allocate buffers, start, ...).
- **Formats.** omacam asks for XRGB8888 (32-bit linear, the safe GPU import
  format); libcamera adjusts the sizes slightly (2016x1136 becomes
  2008x1136). A hardware ISP with YUV output adjusts the format instead:
  YUYV and NV12 frames are taken as they are (with the stream's matrix and
  range), drawn by GTK as YUV memory textures (GTK 4.20), recorded as YUY2
  or NV12, and turned into RGB only for photos, merges, metering and the
  histogram (`omacam_core::pixels`).
- **A video stream**, where the lens table names one (`video`, the Moto
  G60s's 1920x1080 from the full sensor width): video mode streams the
  camera's video role at that size instead of the preview, and the
  viewfinder and the recording take its frames.

## Zoom and lens switching (§3.4)

Zoom is relative to the main camera. The Pixel 7 Pro's factors come from
Google's published fields of view: ultrawide 0.45x, main 1x, telephoto
4.78x (`data/cheetah.toml`), not yet calibrated on our own frames.

A lens shows any zoom at or above its own factor by cropping its frame; it
can't show less. So a switch down to a wider lens happens as soon as the
zoom drops below the current lens's factor, and the 6 % hysteresis sits
above each threshold: from the main camera, the telephoto takes over at
4.78 × 1.06 = 5.07x during a pinch. (The plan puts the band below the
threshold, which a lens can't display.) A pinch or a slider drag switches
only after the zoom has rested for 150 ms; a preset button switches at once
and without hysteresis, so the 5 button lands on the telephoto.

While a new lens starts, the last frame stays on screen and the zoom keeps
applying to it, so the picture follows the fingers; when the new lens's
first frame arrives, the old frame fades out over it in 180 ms instead of
the picture cutting over.

Zoom within a lens is a crop in the render transform (the software ISP
doesn't apply `ScalerCrop`); the photo applies the same crop to the
full-resolution frame, and the recorder to the video.

### Framing every lens alike (`framing.rs`)

A switch jumps when the two lenses frame the scene differently. Three
things decide the framing, all in the lens table:

- **The zoom factor** (`zoom`): the lens's field of view against the main
  camera's, from Google's published fields of view.
- **What the preview mode covers** (`coverage`): the main camera's
  2016x1136 preview is the 2x2-binned middle of its 4080x3072 sensor: 99 %
  of its width but only 74 % of its height (derived from the preview and
  still sizes unless the table says otherwise). Drawn at its true size
  inside the lens's whole 4:3 picture, it would frame the scene exactly as
  the other lenses do, but near 1x it leaves empty bands at the sides of
  the main camera's view, the app's default view, which looks broken. So
  by default a preview that covers less than its picture **fills the view**
  instead (`Lens::shown_coverage`), as in 0.2.0, and the costs are known:
  - **The photo shows more than the preview on the main camera.** At 1x
    the preview shows 74 % of the photo's width (all of its height in the
    Full aspect, 75 % of it in 4:3); the photo takes the main camera's
    whole 4:3 view. In the 16:9 aspect the two match. Zoomed in, the
    photo's crop is the zoom's share of the whole 4:3 view, so it stays
    about a third wider than the preview.
  - **The framing jumps at the main camera's switches** (0.99x to 1x, 4.77x
    to 4.78x) by that third; the cross-fade softens it.

  The drawing, tap to focus, the photo and the recorder already work in
  the picture's terms, so true framing turns on by itself for a lens whose
  preview covers its whole picture (a 4:3 preview mode, CAMERA-PLAN
  CAM-M.5b), or with `true_framing = true` in the lens table. libcamera
  doesn't report a mode's sensor crop yet, so it can't be detected from
  the configured camera.
- **Where each lens points** (`axis`): the lenses sit a centimeter or two
  apart, so their pictures' centers differ. The view's center slides from
  one lens's axis to the next one's as the zoom goes from the one's factor
  to the other's (on the logarithm of the zoom), so at a switch the new
  lens is centered where the old one's crop was. The axes are the center
  until measured on our own frames (P4); the machinery and its tests are in
  place.

The viewfinder, tap to focus, photos (`framing::frame_window`) and the
recorder all use the same transform, so with true framing what the screen
shows is what the file gets (videos always match the screen: they record
the preview's own frames).

### Low light (`lowlight.rs`)

The telephoto (f/3.5) gets about 3.6 times less light than the main camera
(f/1.85). Stock falls back to a crop of the main camera when the light is
too low for it, and so does omacam: exposure time times gain (analog and
digital) from each preview frame, scaled by the square of the f-number
ratio, says what the telephoto would need. Past 1/30 s at gain 8 (ISO 400)
the scene is dim, and the main camera shows the telephoto's zoom with a
"Low light: main camera" note; below half of that (a stop of hysteresis)
it is bright again. Each must last (1.5 s to fall back, 3 s to come back)
before anything switches, so the exposure settling after a switch doesn't
flap the lenses. Any back lens judges the light, so 5x in the dark goes
straight to the main camera.

- **Only where the zoom picks the lens** (the setting "Switch lenses while
  zooming"; by default the lens is the one the user chose and never
  changes on its own), and never with the manual controls open: there
  the user is choosing the camera's settings by hand. Closing them
  restores the fallback. The setting "Low-light lens" turns it off.
- **Not for the ultrawide:** the main camera can't show a wider view than
  its own, so falling back would change the framing, and f/2.2 is close
  to the main camera's f/1.85 anyway.
- **Not while recording or taking a photo:** the light isn't judged then,
  so a video doesn't change lens on its own.

## Display (§3.2)

Each frame becomes a texture drawn by the `Viewfinder` widget: turned
upright, mirrored for the front camera, scaled by the zoom crop, and fitted
whole into the area above the control bar (the picture being the lens's
whole field of view; see Framing above). libcamera's `Rotation` property
is the correction to apply *counter-clockwise* (`property_ids_core.yaml`,
"Example two - Phone camera"), so a frame is turned 360 - Rotation degrees
clockwise: on today's device tree the main and front cameras (Rotation 90)
turn 270° clockwise, the ultrawide and telephoto (270) turn 90°. Photos are
turned the same way before encoding.

### Turning the controls

Held sideways, the layout stays where it is on the phone and the controls
turn in place, as Android cameras do. Where the shell turns the whole
screen (Phosh: phoc turns the output and the window becomes landscape),
the window's content is turned back (`turned.rs`): laid out in the
portrait shape and turned a quarter, so the shutter stays at the same
edge of the phone and the picture fills its width there; GTK draws and
picks input through the turn. Which quarter follows how the phone is
held (the accelerometer, the shell's own cue). Phosh can't keep one
window out of its turn. The quick settings, the lens buttons, the
gallery and flip buttons, the manual chips, the zoom readout and the
toasts turn a quarter (CSS `transform` with a 250 ms transition, from the
accelerometer's orientation; `--hold`, or the O key with `--fake`, on a
laptop). The banners over the top of the picture (recording time, AE/AF
lock, low light) move to the edge that is up and run along it. No word
lies on its side: the mode bar shows its modes as symbols (Night, Photo,
Video), which turn too, and names the chosen one beside its symbol only
while the phone is upright; the line of manual values set by hand shows
only then as well (the adjust button's accent color says so either way).
The settings sheet follows the screen.

### The layout

The picture is fitted to the window's width (the limiting side of a
phone held upright) and never cropped beyond what the photo gets. It sits
between the quick settings at the top and the lens row, mode bar and
shutter row at the bottom when it fits there (a 4:3 photo on a 20:9
screen); a taller one (16:9, video) starts under the top bar and runs
under the bottom bar, which is half see-through
(`viewfinder::fit`). The manual controls open over the bottom of the
picture.

- **Top:** flash (its four choices drop down under it; a long press
  switches the torch), self-timer, aspect ratio (its four choices drop
  down too), HDR (where the lens table allows bursts) or 60 fps in video,
  the settings gear. These start afresh at every launch, as do the lens,
  the zoom and the manual values (flash auto, no timer, the full frame,
  the main camera at 1x, all automatic): what stays across launches is
  what the gear's sheet sets. Within a run, leaving the app and coming
  back keeps them all; only a tapped point is given up.
- **Lens row** (`lensrow.rs`): the zoom field, the zoom relative to the
  main camera's view whichever lens is chosen (`ViewState::zoom_unit`):
  "1.0×", the ultrawide's own view its factor, "0.5×", and its 1× where
  it shows what the main camera shows at 1×. The macro lens, off the
  ladder, reads relative to its own view. A tap
  on it, or a sideways drag across it, shows the zoom dial (`dial.rs`) in
  its place, a ruler of zoom values under a marker with the lens's limits
  and its 1× marked, a light detent at each and a tap on a mark going
  there; the dial closes 2.5 s after the last touch or at a tap
  elsewhere. Its writing turns with the phone. Zooming crops the chosen
  lens within its limits (`min_zoom`/`max_zoom` per lens in the lens
  table, relative to its own view; by default from its view up to the
  table's `max_zoom`), never changing it. Beside the stages the lens button, a ring with
  the chosen lens's symbol (`ViewState::lenses`); a tap drops the lens
  menu over the row, each back lens with its symbol and name, and a
  choice (`Controller::choose_lens`) opens that lens at its own view and
  shows its name over the picture for a moment. The macro lens is one of
  them. A pinch shows its zoom in the zoom field, not over the picture. At the row's end the adjust button for the manual controls.
- **Mode bar** (`modebar.rs`): one line, Night (where the lens table
  allows bursts), Photo, Video; a tap or a swipe chooses, and the line
  slides so the chosen mode sits in the middle.
- **Shutter row:** the last photo, the shutter, the camera switch.

The lens changes only when the user picks one (`Options::hold_lens`, the
default in the app): a pinch, the dial or the mouse wheel crops the
chosen lens, from its own field of view up to the table's `max_zoom`.

### The flash

The top bar's flash button shows the mode (auto, on, off, torch) and drops
its four choices down under it; a long press switches the torch on or
back to the mode before it. The controller does the rest (see Flash
below). In video only the torch or nothing applies. The torch isn't kept
across launches: the setting keeps the mode it was switched on from, so
the app never starts with the light on.

### Display paths

Two paths, `--display`:

- **memory** (default): the backend copies the frame and requeues its buffer
  at once; the UI wraps the copy in a `GdkMemoryTexture`.
- **dmabuf**: the backend lends the buffer's dmabuf; the UI wraps it in a
  `GdkDmabufTexture` whose release callback returns the buffer. Before a
  stream stops, the frozen frame is downloaded into memory so every buffer
  is back before the allocator frees it. Works on the Pixel 7 Pro with Mesa
  26.2.4-1.2; it becomes the default once the panthor export fix is in the
  released Mesa package.

## Video (§3.9)

### What the hardware and the system offer (2026-10-14)

- **No hardware encoder.** The gs201's video codec block (MFC) has no
  mainline driver in our kernel: no `/dev/video*` encoder node (the four
  video nodes are the CSIS capture DMAs), no `v4l2h264enc`. Encoding is on
  the CPU: two Cortex-X1, two A78 and four A55 cores.
- **Encoders on the phone:** the x264, OpenH264 and libvpx libraries are
  installed; GStreamer has `vp8enc`/`vp9enc` (gst-plugins-good) but not
  `x264enc` (gst-plugins-ugly) or `openh264enc` (gst-plugins-bad). The
  omacam package depends on gst-plugins-ugly, which Arch Linux ARM ships
  (1.28.8, on x264).
- **Choice:** H.264 by x264 at `speed-preset=ultrafast tune=zerolatency`,
  baseline profile, about 12 Mb/s at 1080p30 and 5.5 Mb/s at 720p30, with
  Opus sound (Q11), in MP4. x264's ultrafast is the cheapest software H.264
  there is and plays everywhere; VP8/VP9 cost more CPU per frame at the
  same quality, so VP8 in WebM is only the fallback when no H.264 encoder
  is installed (`omacam_record::best_codec`). OpenH264 sits between them.
  Measured on the Pixel 7 Pro (2026-10-14, libcamera 0.7.2-4.8), the main
  camera through `libcamerasrc` into the encoder chain with no UI: camera
  alone 30.2 fps; with crop, `videoconvertscale` (4 threads) and x264 at
  1080p 30.1 fps, at 720p 30.2 fps. Synthetic 2008x1136 frames through the
  same chain: 36 fps at 1080p, 41 at 720p. So the encoder keeps up at 30.
- **The container.** MP4 carries Opus (the ISO BMFF Opus mapping):
  mpv (the phone's video player), Firefox, Chromium, VLC and GStreamer play
  it; QuickTime doesn't. Matroska would carry it as well but plays in fewer
  galleries and editors; MP4 it is.
- **Sound** comes from PipeWire's default source (the AoC's microphones),
  `pipewiresrc` into `opusenc` at 48 kHz stereo. When no microphone opens,
  the video is recorded without sound and the log says so.
- **Sync.** Frames and sound are both timestamped on the pipeline clock as
  they arrive (`appsrc do-timestamp`, a live `pipewiresrc`), so they stay
  together to within a frame plus the camera's own latency (about one
  frame). The sensor timestamps would tighten that later.
- **60 fps** (2026-10-14, from the libcamera work): none of the cameras
  offers it yet. libcamera's simple pipeline reports no
  `FrameDurationLimits` and the sensor drivers default to 30 fps. A
  libcamera patch is to report the limits per configured mode; then the
  ultrawide and front cameras can run at 60 with no kernel change, and the
  main and telephoto cameras later need a driver change. omacam reads the
  configured mode's `FrameDurationLimits` at each start and offers 60
  only where its minimum is 16.7 ms or less: then video mode asks for
  `FrameDurationLimits` [16667, 16667] (video at 30: [33333, 33333]), a
  "60 FPS" toggle shows beside the gear, and recording runs at 60 only if
  the camera really delivers 50 fps or more, else at 30 with a note.
  Photo mode asks for [33333, the mode's longest frame]: at most 30 fps,
  and in a dim scene the exposure may lengthen frames rather than stop at
  33 ms with the gain at its limit.
- **Orientation.** The encoded frames are landscape; the camera's upright
  rotation goes into the file as its `image-orientation` tag, which mp4mux
  writes as the track's display matrix, so players show it upright.

### How it records

One stream feeds both: the preview's processed frames (2008x1136 on the
main camera, 2008x1508 on the ultrawide) go to the viewfinder and, while
recording, to the encoder as the same shared buffer (no copy). The
encoder is fed on the camera's own thread, as each frame arrives, not
from the viewfinder: 0.1.7 encoded only the frames the UI thread got to
show, and with that thread saturated the owner's first clips came out at
9-17 fps.

Timestamps: every frame is stamped with the pipeline clock's running time
as it arrives, the clock the microphone's sound is stamped with; the
first frame becomes the file's zero (an offset on both tracks), and sound
before the first frame or after the last is dropped, so both tracks
cover the same span. (0.1.7 let appsrc stamp frames once the pipeline
ran, which could put the video seconds after or before the sound.) The
muxer's input queues hold seconds, not the default 200 ms: a short audio
queue that filled while the muxer waited for video had blocked the
microphone and lost sound.

Orientation: the file's rotation is the camera's upright turn plus how
the phone is held when recording starts (`orientation.rs`, from
iio-sensor-proxy's accelerometer; the front camera turns the other way),
so a video recorded in landscape plays in landscape. Photos turn their
pixels the same way at the shutter (EXIF Orientation stays 1), and a 4:3
or 16:9 aspect becomes a landscape one when held sideways. The file's
creation time is when recording started. `videocrop` takes the 16:9 middle of the frame and the zoom crop
(padded rows are cropped off the same way), `videoscale` makes it 1920x1080
or 1280x720 with square pixels (a fixed pixel aspect ratio: a zoom that
changed it renegotiated the encoder, which mp4mux refused), and x264
encodes it. The viewfinder shows the 9:16 frame the video records.

**Switching lenses while recording.** The zoom picks lenses as it does for
photos (back cameras only; flip and photos wait). The recorder keeps
running: when the new camera starts, the camera thread hands the recorder
its frame layout before its first frame (`Tap::set_input`), and the
recorder sets the new caps on `appsrc`. The crop is set per frame where it
meets `videocrop` (a pad probe: on the caps change, then per buffer by its
timestamp), so frames still in flight from the old camera keep the old
camera's crop, and the scale to the file's fixed size takes any frame size.
A camera mounted the other way up (the main camera's frames turn 270°,
the ultrawide's and telephoto's 90°) is turned half a turn by `videoflip`
after scaling, where the frame is smallest, so the file's one orientation
tag fits both. The stop and start of the camera, about 300-600 ms, shows as
the last frame held: timestamps are the clock's, so the gap is real time,
not a jump.

**Stabilization** (`stabilize.rs`), with omamobile-sensors' gyroscope
stream (`org.omamobile.SensorProxy.Gyro1`, claimed at 240 Hz while
recording; SOCK_SEQPACKET records of time, rates, sequence). The video
crops 10 % more than the view (`MARGIN`; the viewfinder in video mode
shows that framing) and the crop moves with the shake: the gyroscope's
rates, integrated, give the phone's turn; the turn smoothed over 0.5 s is
the intended path; the difference, times the focal length (from the 35 mm
equivalent and the picture's shape), is how far the scene moved, and the
crop follows it, up to the margin (when it runs out, the path is pulled
along, so a pan goes through with a short lag). Frames are matched to the
gyroscope on CLOCK_MONOTONIC: gs201's receiver stamps SensorTimestamp at
the end of the frame's DMA, so the exposure's middle is that less half the
readout (an estimated 20 ms until measured per mode) and half the
exposure. The gyroscope's small bias is a slow turn the path follows.
Roll isn't corrected (it needs a rotation), nor is the rolling shutter.
Without the stream (no proxy, an older one, or the front camera), videos
aren't stabilized and the view isn't tightened. Axes: Android's device
frame (x right, y up, z out of the screen); a turn about y moves the scene
sideways, about x up and down; the signs want a check on the phone.

**Location.** With location tags on, the newest GeoClue fix (at most 10
minutes old) goes into photos (EXIF GPS: position, altitude, accuracy, the
fix's UTC time) and videos. mp4mux writes no location from tags, so the
recorder adds a `©xyz` box (ISO 6709, `+51.4779-000.0015/`, as Android and
iOS write it) to the finished file's `moov`, which mp4mux writes last, so
no media moves. Off by default (Q5). GeoClue answers a user's apps only
while an authorization agent runs (omamobile-gnss's Location switch starts
one); the package lists `omacam` in `/etc/geoclue/conf.d/91-omacam.conf` so
the agent isn't asked about it. Tests use only neutral coordinates
(Greenwich, 0/0).

Videos go to
`$XDG_VIDEOS_DIR/Camera` (`~/Videos/Camera`): desktop Linux keeps photos
and videos in their own folders, and the file manager and players look
there; Pixel's single DCIM folder is an Android convention.

## Exposure, white balance and focus (§3.5)

Automatic by default, with manual control where the camera allows it:

- **Tap** focuses and **meters** the tapped area. Where the camera takes
  them (libcamera 4.10's soft ISP), the tap sends AfMode Auto, AfMetering
  Windows with one AfWindows rectangle (at least 10 % of the picture per
  side, in sensor active-area coordinates), AeMeteringMode Spot (which
  meters the same rectangle) and AfTrigger Start. Without AeMeteringMode
  the area is metered by brightness: its mean luma is brought to mid-gray
  (118 of 255) through `ExposureValue`, at most ±2 EV.
- **Autofocus is continuous** by default, in photos and video (AfMode
  Continuous where the camera has autofocus and focus is on auto). The
  software ISP's contrast autofocus (Omarchy Mobile's libcamera patches
  0005-0007) supports it and smooths its sharpness over about eight
  frames, so it holds steady rather than hunting. In photos a tap runs one
  scan on the tapped window (AfMode Auto, AfTrigger Start), holds it for 5
  s, then continuous AF takes over again. In video a tap on a camera whose
  AF takes windows moves continuous AF's window there instead (AfMode
  Continuous, AfMetering Windows, AfWindows), so it keeps following that
  spot without a visible scan; elsewhere it scans once, as in photos.
  Recording starts with one scan. AE/AF lock holds focus with AfPause.
- **The mark.** Four corners of a square, a thin light line with a dark
  edge, so they show on a white wall and in the dark alike and leave the
  subject inside free: larger while AF scans, a little smaller once sharp
  (reddish when it fails).
- **The tapped point stays marked.** After the scan, focus (continuous AF
  on the tapped window) and metering keep following the point, so small,
  fainter corners stay on it (`ViewState::tap_point`). A tap on
  the marker gives the whole picture back to focus and metering
  (`Controller::clear_tap`: AfMetering Auto, matrix metering, no
  correction for the spot); so does the window coming back to the front
  (from the photo viewer, say), a lens switch, a mode switch, and every
  fresh start of the stream.
- **Exposure correction** is in the manual controls, -2..+2 EV on top of
  the metering; a tap or a lens switch resets it. The older slider beside
  the focus ring is a setting ("Exposure slider at the focus point", off
  by default).
- **Long press: AE/AF lock.** Exposure is held with AeEnable false where
  the camera offers it (AE keeps its last values), else the exposure time
  and gain the last frame reported become manual values. Focus is held
  with AfPause Immediate where offered, else at the reported lens
  position (AfMode Manual). An "AE/AF lock" chip shows; a tap releases it.
- **Manual controls** (`manual.rs`, the adjust button beside the lens
  row): exposure correction, focus (AfMode Manual + LensPosition, from far
  to near), white balance, shutter (ExposureTimeMode + ExposureTime,
  within the current mode's frame time) and ISO (AnalogueGainMode +
  AnalogueGain; either can be manual while AE drives the other). Each is
  automatic until its slider moves; "Auto" or a long press on its chip
  gives it back. A value set by hand shows in the accent color with a dot,
  the adjust button turns the accent color, and while the controls are
  closed a line over the lens row names the values set by hand (its ×
  sets all back to auto). White balance is a color temperature on a
  camera that advertises ColourTemperature, else four presets (Bulb, Tube,
  Sun, Cloud) as fixed ColourGains [r, b] with AwbEnable false. An
  automatic value's slider follows what auto chose (dimmed), so taking it
  over starts there. ISO is analog gain × the sensor's base ISO
  (`base_iso` in the lens table). A histogram of the picture's luma can
  be shown over the picture. Manual values hold across lens switches,
  apply to stills and video alike, and reset at each launch.
- **Stills wait for the exposure to settle.** After the switch to the
  still mode, the capture waits for AeState Converged (and the color to
  settle), at most 10 frames; a camera without AeState gets 8 frames.
- **What the camera offers decides what's enabled.** Every control is
  looked up in the camera's ControlInfoMap at startup (logged as "camera
  controls"); a chip whose control is missing is grayed with a tooltip.
  On the phone's libcamera 0.7.2-4.8 the main camera offers AfMode,
  AfTrigger and LensPosition (0-1) and none of the exposure or white
  balance controls, so manual focus works there and the rest waits for
  libcamera 4.10; the 4.10 code paths are tested with a fake camera that
  offers its controls.
- EXIF records what was used: exposure time, ISO, ExposureMode (manual
  when shutter or ISO was set by hand), WhiteBalance, and the exposure
  bias.

## Flash (`flash.rs`)

A phone's flash is a white LED beside the back cameras, driven by the
kernel's LED class (`/sys/class/leds`). omacam uses its steady light, not
its strobe: on the Moto G60s the device tree caps the strobe at the torch
current (175 mA) and 384 ms, the camera makes each photo from a series of
frames over one to two seconds, and nothing ties a strobe to a frame's
exposure. A photo with the flash is the usual LED-flash sequence: light on,
wait under it until the exposure and the white balance have settled, take
the photo as usual (from the stream beside the preview, or the still mode),
light off. The exposure must first have seen the light (exposure time
times gain under 80 % of the one before it, or AeState Searching; with no
change for 1 s the light doesn't reach the scene): on the Moto G60s AE
still said Converged 300 ms after the light came on, at the old exposure,
and that photo came out white. Then the photo is taken once AeState is no
longer Searching with the exposure steady over two frames and the color
gains steady, or 0.7 s after the reaction; at least four frames and 300 ms,
at most 2.5 s. On the G60s the exposure loop sees the light after 300-500
ms and then walks down for over 2 s, while its still stream sets an
exposure of its own: photos 0.6 s after the reaction came out as well
exposed as those after 1.6 s.

- **Modes** (`FlashMode`, the flash button; every launch starts at auto):
  off, auto (lit for a photo when the preview's exposure time times its
  gain is above the lens table's `auto_above`), on (every photo), and
  torch, a steady light on every back lens (the macro camera as a
  magnifier) that stays on through photos. The front camera has no flash:
  the mode is kept and nothing lights there.
- **The LED goes out** as soon as the window leaves the front (unfocused,
  hidden, the screen locked or off, the photo viewer opened:
  `Controller::set_foreground`), well before the cameras are let go, and
  the torch comes back on with the window. A flash photo still waiting
  under its light is given up; one being taken finishes without it. It
  goes out too on the front camera, and while the camera fails (until it
  streams again). The controller says what the LED should do
  (`Output::Light`: off, torch, photo) and the app sets it, only when
  that changes.
- **Rights.** The LED's files belong to root. logind sets an LED's
  brightness for the user of the active graphical session
  (`org.freedesktop.login1.Session.SetBrightness("leds", NAME, VALUE)`), as
  Phosh's torch does; omacam calls it on the user's display session
  (`org.freedesktop.login1.User.Display` of the user with the app's uid),
  since the app runs in a systemd user scope outside any session, or, started
  with su or over ssh, in a session without a seat.
- **A crash or a kill can't leave it on.** A helper process (`omacam
  --led-helper NAME`) makes the calls, fed brightness values through a pipe;
  when the app ends in any way, the pipe closes and the helper puts the LED
  out. It ignores SIGINT, SIGTERM and SIGHUP and waits for the pipe.
- **After the torch.** The torch out, the exposure walks back from the
  light; a photo taken at once would get the exposure set for it (on the
  Moto G60s it came out dark). So a photo asked for then waits the same
  way, until the exposure has seen the dark (its demand up by a quarter)
  and settled, at most 2.5 s.
- **EXIF** records the Flash tag: fired or not, and the mode (the torch
  counts as fired).
- **Temperature** is the kernel's to guard (the LED as a cooling device):
  when it dims the LED or puts it out, the app leaves it so and doesn't
  set it again until the light it wants changes.
- **The lens table** (`[flash]`): `led` (the LED's name, `""` for none;
  left out, an LED whose name says flash or torch), `torch` and `photo`
  (brightness steps; the LED's maximum when left out), `auto_above`.

## Sharing the cameras

libcamera locks a camera per process. omacam releases every camera and its
camera manager when the window is suspended (GTK's toplevel `suspended`
state) or has been unfocused for 10 s, and opens them again when it comes
back. A camera that fails to start (another app has it) is retried every
2 s.

## Theme

`crates/omacam/src/theme.rs` follows the Omarchy theme as gliff does: it
reads `~/.local/state/omarchy/current/theme/colors.toml`, maps the palette
onto libadwaita's CSS variables, forces the light or dark scheme, and
watches the directory for theme changes. The viewfinder stays black; the
active zoom button and the busy shutter take the theme's accent.

## Not built yet

- The in-app viewer of recent shots, RAW, a level, HDR and night modes.
- A stabilized preview (the file is stabilized; the viewfinder shows its
  framing, not its steadiness), roll correction.
- Exposure carried across a lens switch: computed (`exposure.rs`), passed to
  libcamera only with `--exposure-hints` until it's checked whether the
  simple IPA takes `ExposureTime` as a starting point or as manual exposure.
- Calibration of the zoom factors, lens axes and the sensors' readout
  times (P4); the code takes them from the lens table.
