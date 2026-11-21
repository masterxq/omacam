//! omacam's libcamera backend, through libcamera-rs.
//!
//! libcamera-rs's `ActiveCamera` isn't `Send` (libcamera-rs #86), so the
//! camera manager, the camera and its buffers all live on one thread of
//! ours. Commands, completed requests and buffer returns reach that thread
//! through one channel, so it handles them in order.
//!
//! Display modes:
//! - [`DisplayMode::Memory`] copies each frame out of the camera's buffer
//!   and requeues the buffer at once. One copy of about 9 MB a frame at
//!   2016x1136, but it doesn't touch the GPU import path (the default
//!   until Mesa's panthor export fix lands).
//! - [`DisplayMode::Dmabuf`] lends the camera's dmabuf to the UI and
//!   requeues the request only when the UI drops the frame's
//!   [`ReleaseToken`].
//!
//! A camera that gives NV12 instead of the XRGB8888 asked for (a hardware
//! ISP with YUV output, such as the MediaTek DIP behind libcamera's simple
//! pipeline) has its frames converted to XRGB8888 on the CPU as they are
//! copied out, in either display mode.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use libcamera::camera::ActiveCamera;
use libcamera::camera_manager::CameraManager;
use libcamera::color_space::{ColorSpace, Range, YcbcrEncoding};
use libcamera::control::{ControlEntry, ControlInfoMap, ControlList};
use libcamera::control_value::ControlValue;
use libcamera::framebuffer::{AsFrameBuffer, FrameMetadataStatus};
use libcamera::framebuffer_allocator::{FrameBuffer, FrameBufferAllocator};
use libcamera::framebuffer_map::MemoryMappedFrameBuffer;
use libcamera::geometry::{Rectangle, Size as LcSize};
use libcamera::pixel_format::PixelFormat;
use libcamera::request::{Request, RequestStatus, ReuseFlag};
use libcamera::stream::{Stream, StreamRole};
use libcamera::{controls, properties};

use omacam_core::backend::{
    AeState, AfState, Awb, AwbPreset, Backend, CameraInfo, Caps, Command, Controls, Event,
    EventSink, ExposureSettings, Frame, FrameData, FrameMeta, IspMeta, Location, PixelLayout,
    Purpose, RawFrame, Rect, ReleaseToken, Setting, Size, StreamRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    #[default]
    Memory,
    Dmabuf,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub display: DisplayMode,
    /// Pass exposure and gain hints to a starting stream. Off by default:
    /// whether the simple IPA takes ExposureTime as a starting point or as
    /// manual exposure needs checking on the phone first (CAMERA-APP-PLAN
    /// L3).
    pub exposure_hints: bool,
}

/// XRGB8888, the format asked of the software ISP: 32-bit linear, the
/// safe EGL import target on panfrost.
const XRGB8888: PixelFormat = PixelFormat::new(u32::from_le_bytes(*b"XR24"), 0);
/// The raw formats the raw merge reads (MIPI CSI-2 packed 10-bit), with
/// where each puts its red sample: (column, row).
const RAW_FORMATS: [(PixelFormat, (u8, u8)); 4] = [
    (libcamera::formats::SRGGB10_CSI2P, (0, 0)),
    (libcamera::formats::SGRBG10_CSI2P, (1, 0)),
    (libcamera::formats::SGBRG10_CSI2P, (0, 1)),
    (libcamera::formats::SBGGR10_CSI2P, (1, 1)),
];
const EINVAL: i32 = 22;
const ENOMEM: i32 = 12;

/// A failed start, and the step that failed.
#[derive(Debug)]
struct StartError {
    step: &'static str,
    error: io::Error,
}

impl StartError {
    fn is_out_of_memory(&self) -> bool {
        self.error.raw_os_error() == Some(ENOMEM)
    }
}

/// How often to look for cameras while there are none.
const POLL_CAMERAS: Duration = Duration::from_secs(2);
/// How long a stop waits for the UI to give back lent buffers.
const LENT_WAIT: Duration = Duration::from_millis(300);

enum Msg {
    Cmd(Command),
    /// libcamera added or removed a camera (`removed`: its id).
    CamerasChanged {
        removed: Option<String>,
    },
    Completed(Request),
    Release {
        generation: u64,
        cookie: u64,
    },
    Quit,
}

pub struct LibcameraBackend {
    tx: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl LibcameraBackend {
    pub fn spawn(sink: EventSink, opts: Options) -> io::Result<LibcameraBackend> {
        let (tx, rx) = mpsc::channel();
        let me = tx.clone();
        let thread = std::thread::Builder::new()
            .name("omacam-libcamera".into())
            .spawn(move || Worker::new(me, sink, opts).run(rx))?;
        Ok(LibcameraBackend {
            tx,
            thread: Some(thread),
        })
    }
}

impl Backend for LibcameraBackend {
    fn send(&self, command: Command) {
        let _ = self.tx.send(Msg::Cmd(command));
    }
}

impl Drop for LibcameraBackend {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type MappedBuffer = MemoryMappedFrameBuffer<FrameBuffer>;

/// A configured, running camera. Field order is drop order: the camera
/// stops and releases before its buffers go.
struct Active {
    cam: ActiveCamera<'static>,
    generation: u64,
    camera_id: String,
    stream: Stream,
    size: Size,
    stride: u32,
    layout: PixelLayout,
    /// Copy frames out (memory display, and always for stills, which the
    /// encoder reads on the CPU, and for NV12, which is converted).
    copy: bool,
    /// The camera gives NV12: frames are converted to XRGB8888.
    nv12: Option<Nv12>,
    /// The camera takes AwbMode (the simple pipeline doesn't, and logs an
    /// error for each request that sets it).
    awb_modes: bool,
    /// Completed requests lent to the UI (dmabuf mode), by cookie.
    lent: HashMap<u64, Request>,
    /// Requests that couldn't be queued.
    idle: Vec<Request>,
    /// Controls to put on the next queued request.
    pending_controls: Option<Controls>,
    /// The configured mode's highest frame rate (FrameDurationLimits).
    max_fps: Option<f64>,
    /// The raw stream beside the processed one, when the start asked for it
    /// and the camera gives it at the same size.
    raw: Option<RawStream>,
    /// The sensor's model, for the raw frames' tuning file.
    sensor_model: Option<String>,
    _alloc: FrameBufferAllocator,
}

/// NV12 frames from the camera, and how to turn them into XRGB8888.
#[derive(Debug, Clone, Copy)]
struct Nv12 {
    /// Line length of the Y and of the CbCr plane, bytes.
    stride: u32,
    /// YCbCr to RGB in 16.16 fixed point: Y scale, Cr to R, Cb and Cr to G,
    /// Cb to B.
    y_scale: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
    /// Black level of Y: 16 in limited range, 0 in full range.
    y_offset: i32,
}

impl Nv12 {
    /// BT.709 or BT.601 coefficients, limited or full range, from the
    /// stream's color space; BT.601 full range (as JPEG) where the
    /// pipeline names none.
    fn new(stride: u32, cs: Option<ColorSpace>) -> Nv12 {
        let rec709 = cs.is_some_and(|c| c.ycbcr_encoding == YcbcrEncoding::Rec709);
        let limited = cs.is_some_and(|c| c.range == Range::Limited);
        let (kr, kb) = if rec709 {
            (0.2126, 0.0722)
        } else {
            (0.299, 0.114)
        };
        let kg = 1.0 - kr - kb;
        let (ys, cs) = if limited {
            (255.0 / 219.0, 255.0 / 224.0)
        } else {
            (1.0, 1.0)
        };
        let fixed = |v: f64| (v * 65536.0).round() as i32;
        Nv12 {
            stride,
            y_scale: fixed(ys),
            rv: fixed(2.0 * (1.0 - kr) * cs),
            gu: fixed(2.0 * (1.0 - kb) * kb / kg * cs),
            gv: fixed(2.0 * (1.0 - kr) * kr / kg * cs),
            bu: fixed(2.0 * (1.0 - kb) * cs),
            y_offset: if limited { 16 } else { 0 },
        }
    }
}

struct RawStream {
    stream: Stream,
    size: Size,
    stride: u32,
    first_red: (u8, u8),
}

struct Worker {
    me: mpsc::Sender<Msg>,
    sink: EventSink,
    opts: Options,
    active: Option<Active>,
    manager: Option<CameraManager>,
    /// Messages that arrived while a stop drained the channel.
    deferred: VecDeque<Msg>,
    /// The camera ids last announced, and whether any list went out.
    known: Vec<String>,
    announced: bool,
}

impl Worker {
    fn new(me: mpsc::Sender<Msg>, sink: EventSink, opts: Options) -> Worker {
        Worker {
            me,
            sink,
            opts,
            active: None,
            manager: None,
            deferred: VecDeque::new(),
            known: Vec::new(),
            announced: false,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        if let Err(e) = self.open_manager() {
            tracing::warn!(error = %e, "libcamera didn't start; waiting for cameras");
        }
        self.announce_cameras();
        loop {
            let msg = match self.deferred.pop_front() {
                Some(m) => m,
                // No cameras yet (the drivers load late in boot): look
                // again every couple of seconds.
                None if self.known.is_empty() && self.active.is_none() => {
                    match rx.recv_timeout(POLL_CAMERAS) {
                        Ok(m) => m,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            self.manager = None;
                            let _ = self.open_manager();
                            self.announce_cameras();
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                None => match rx.recv() {
                    Ok(m) => m,
                    Err(_) => break,
                },
            };
            match msg {
                Msg::Quit => break,
                Msg::CamerasChanged { removed } => {
                    if let Some(id) = removed {
                        tracing::warn!(camera = id, "camera removed");
                        if self.active.as_ref().is_some_and(|a| a.camera_id == id) {
                            self.stop(&rx);
                        }
                    }
                    self.announce_cameras();
                }
                Msg::Cmd(cmd) => self.command(cmd, &rx),
                Msg::Completed(req) => self.completed(req),
                Msg::Release { generation, cookie } => self.release(generation, cookie),
            }
        }
        self.stop(&rx);
        self.manager = None;
    }

    fn open_manager(&mut self) -> io::Result<()> {
        if self.manager.is_none() {
            let mut mgr = CameraManager::new()?;
            let me = self.me.clone();
            mgr.on_camera_added(move |_| {
                let _ = me.send(Msg::CamerasChanged { removed: None });
            });
            let me = self.me.clone();
            mgr.on_camera_removed(move |cam| {
                let _ = me.send(Msg::CamerasChanged {
                    removed: Some(cam.id().to_string()),
                });
            });
            self.manager = Some(mgr);
        }
        Ok(())
    }

    /// Send the camera list when it differs from the last one sent.
    fn announce_cameras(&mut self) {
        let cameras = self.list_cameras();
        let ids: Vec<String> = cameras.iter().map(|c| c.id.clone()).collect();
        if ids == self.known && self.announced {
            return;
        }
        tracing::info!(count = cameras.len(), "cameras found");
        self.known = ids;
        self.announced = true;
        (self.sink)(Event::Cameras(cameras));
    }

    fn list_cameras(&self) -> Vec<CameraInfo> {
        let Some(mgr) = &self.manager else {
            return Vec::new();
        };
        let list = mgr.cameras();
        list.iter()
            .map(|cam| {
                let props = cam.properties();
                let location = match props.get::<properties::Location>() {
                    Ok(properties::Location::CameraFront) => Location::Front,
                    Ok(properties::Location::CameraBack) => Location::Back,
                    _ => Location::External,
                };
                let rotation = props
                    .get::<properties::Rotation>()
                    .map(|r| r.0.rem_euclid(360) as u32)
                    .unwrap_or(0);
                let model = props
                    .get::<properties::Model>()
                    .map(|m| m.0)
                    .unwrap_or_default();
                let ctrls = cam.controls();
                let has = |id: u32| ctrls.find(id).is_ok();
                // The coordinates AfWindows use: ScalerCropMaximum where the
                // camera reports it, else (libcamera 4.10's simple pipeline)
                // the sensor's active area, from its origin.
                let crop_maximum = props
                    .get::<properties::ScalerCropMaximum>()
                    .ok()
                    .map(|r| Rect {
                        x: r.0.x,
                        y: r.0.y,
                        width: r.0.width,
                        height: r.0.height,
                    })
                    .or_else(|| {
                        let areas = props.get::<properties::PixelArrayActiveAreas>().ok()?;
                        let a = areas.0.first()?;
                        Some(Rect {
                            x: 0,
                            y: 0,
                            width: a.width,
                            height: a.height,
                        })
                    });
                let caps = caps(ctrls);
                tracing::info!(camera = cam.id(), ?caps, "camera controls");
                CameraInfo {
                    id: cam.id().to_string(),
                    location,
                    rotation,
                    model,
                    autofocus: has(<controls::AfMode as ControlEntry>::ID),
                    af_windows: has(<controls::AfWindows as ControlEntry>::ID),
                    crop_maximum,
                    caps,
                }
            })
            .collect()
    }

    fn command(&mut self, cmd: Command, rx: &mpsc::Receiver<Msg>) {
        match cmd {
            Command::Start {
                generation,
                camera,
                stream,
                controls,
            } => {
                self.stop(rx);
                let began = Instant::now();
                match self.start_with_fallbacks(generation, &camera, &stream, &controls) {
                    Ok(note) => {
                        let a = self.active.as_ref().expect("started");
                        tracing::info!(
                            generation,
                            camera,
                            size = %a.size,
                            stride = a.stride,
                            ms = began.elapsed().as_millis() as u64,
                            "stream started"
                        );
                        (self.sink)(Event::Started {
                            generation,
                            camera,
                            size: a.size,
                            stride: a.stride,
                            layout: a.layout,
                            max_fps: a.max_fps,
                        });
                        if let Some(message) = note {
                            (self.sink)(Event::Notice { message });
                        }
                    }
                    Err(e) => {
                        tracing::error!(generation, camera, step = e.step, error = %e.error, "camera didn't start");
                        (self.sink)(Event::Error {
                            generation: Some(generation),
                            message: format!("Camera didn't start ({}): {}", e.step, e.error),
                        });
                    }
                }
            }
            Command::Stop { generation } => {
                if self
                    .active
                    .as_ref()
                    .is_some_and(|a| a.generation == generation)
                {
                    self.stop(rx);
                } else {
                    // Already gone (a camera that went away): say so, so
                    // the app doesn't wait for it.
                    (self.sink)(Event::Stopped { generation });
                }
            }
            Command::SetControls {
                generation,
                controls,
            } => {
                if let Some(a) = self.active.as_mut().filter(|a| a.generation == generation) {
                    // Two changes before the next request both apply.
                    a.pending_controls = Some(match a.pending_controls.take() {
                        Some(older) => older.merged(controls),
                        None => controls,
                    });
                }
            }
            Command::Release => {
                self.stop(rx);
                if self.manager.take().is_some() {
                    tracing::info!("camera manager released");
                }
            }
        }
    }

    /// Start `req`, trying again when memory runs short: once more after
    /// releasing the camera manager, then with one buffer, then at each
    /// fallback size. A still stream also falls back on other start
    /// errors. Returns a note for the user when it had to settle for less.
    fn start_with_fallbacks(
        &mut self,
        generation: u64,
        camera: &str,
        req: &StreamRequest,
        controls: &Controls,
    ) -> Result<Option<String>, StartError> {
        let mut attempts: Vec<(Option<Size>, u32)> = vec![(req.size, req.buffers)];
        attempts.push((req.size, req.buffers));
        if req.buffers > 1 {
            attempts.push((req.size, 1));
        }
        for &f in &req.fallbacks {
            attempts.push((Some(f), req.buffers.min(2)));
        }
        let mut last = None;
        for (i, &(size, buffers)) in attempts.iter().enumerate() {
            let fallback = i > 0;
            if fallback {
                tracing::warn!(
                    generation,
                    camera,
                    size = %size.map(|s| s.to_string()).unwrap_or_else(|| "default".into()),
                    buffers,
                    "retrying the start"
                );
            }
            match self.start(generation, camera, size, buffers, req, controls) {
                Ok(()) => {
                    let got = self.active.as_ref().map(|a| a.size);
                    let note = match (size == req.size, got) {
                        (false, Some(g)) => Some(format!(
                            "Not enough camera memory for the full mode: used {g} instead"
                        )),
                        _ => None,
                    };
                    return Ok(note);
                }
                Err(e) => {
                    tracing::warn!(
                        generation,
                        camera,
                        step = e.step,
                        error = %e.error,
                        errno = e.error.raw_os_error(),
                        attempt = i + 1,
                        "start failed"
                    );
                    let retry = e.is_out_of_memory() || req.purpose == Purpose::Still;
                    if i == 0 && e.is_out_of_memory() {
                        // Let go of everything libcamera holds, then retry.
                        self.manager = None;
                    }
                    last = Some(e);
                    if !retry {
                        break;
                    }
                }
            }
        }
        Err(last.expect("at least one attempt"))
    }

    fn start(
        &mut self,
        generation: u64,
        camera: &str,
        size_wanted: Option<Size>,
        buffers: u32,
        req: &StreamRequest,
        controls: &Controls,
    ) -> Result<(), StartError> {
        let at = |step: &'static str| move |error: io::Error| StartError { step, error };
        self.open_manager().map_err(at("camera manager"))?;
        let mgr = self.manager.as_ref().expect("opened");
        let cam = mgr.get(camera).ok_or_else(|| {
            at("find camera")(io::Error::new(io::ErrorKind::NotFound, "no such camera"))
        })?;
        let mut cam: ActiveCamera<'static> = cam.acquire().map_err(at("acquire"))?;

        // A raw stream beside the processed one, where the camera gives one
        // at the processed frame's size in a format the raw merge reads;
        // otherwise the processed stream alone.
        let mut want_raw = req.raw;
        let (mut cfgs, status, raw_layout) = loop {
            let roles: &[StreamRole] = if want_raw {
                &[StreamRole::ViewFinder, StreamRole::Raw]
            } else {
                &[StreamRole::ViewFinder]
            };
            let Some(mut cfgs) = cam.generate_configuration(roles) else {
                if want_raw {
                    want_raw = false;
                    continue;
                }
                return Err(at("generate configuration")(io::Error::other(
                    "no configuration",
                )));
            };
            {
                let mut sc = cfgs.get_mut(0).expect("one stream");
                sc.set_pixel_format(XRGB8888);
                if let Some(s) = size_wanted {
                    sc.set_size(LcSize::new(s.width, s.height));
                }
                sc.set_buffer_count(buffers.max(1));
            }
            if want_raw {
                let mut rc = cfgs.get_mut(1).expect("a raw stream");
                if let Some(s) = size_wanted {
                    rc.set_size(LcSize::new(s.width, s.height));
                }
                rc.set_buffer_count(buffers.max(1));
            }
            let status = cfgs.validate();
            tracing::info!(
                generation,
                camera,
                purpose = ?req.purpose,
                asked = %size_wanted.map(|s| s.to_string()).unwrap_or_else(|| "default".into()),
                buffers,
                raw = want_raw,
                status = ?status,
                config = %cfgs.to_string_repr(),
                "configuration validated"
            );
            if !want_raw {
                break (cfgs, status, None);
            }
            let layout = (!status.is_invalid()).then(|| raw_layout(&cfgs)).flatten();
            if layout.is_none() {
                tracing::warn!(
                    generation,
                    camera,
                    "no raw stream to match the processed one: bursts merge the processed frames"
                );
                want_raw = false;
                continue;
            }
            break (cfgs, status, layout);
        };
        if status.is_invalid() {
            return Err(at("validate")(io::Error::from_raw_os_error(EINVAL)));
        }
        let (layout, size, stride, frame_size, count, nv12) = {
            let sc = cfgs.get(0).expect("one stream");
            let fmt = sc.get_pixel_format();
            let s = sc.get_size();
            // NV12 where the camera has no RGB output: converted to
            // XRGB8888 as each frame is copied out.
            let nv12 = (fmt == libcamera::formats::NV12)
                .then(|| Nv12::new(sc.get_stride(), sc.get_color_space()));
            let layout = match nv12 {
                Some(_) => PixelLayout::Xrgb8888,
                None => PixelLayout::from_fourcc(fmt.fourcc())
                    .filter(|_| fmt.modifier() == 0)
                    .ok_or_else(|| {
                        at("format")(io::Error::other(format!(
                            "unsupported pixel format {:#x}",
                            fmt.fourcc()
                        )))
                    })?,
            };
            let stride = match nv12 {
                Some(_) => s.width * layout.bytes_per_pixel(),
                None => sc.get_stride(),
            };
            (
                layout,
                Size::new(s.width, s.height),
                stride,
                sc.get_frame_size(),
                sc.get_buffer_count(),
                nv12,
            )
        };
        if let Some(n) = &nv12 {
            tracing::info!(
                generation,
                camera,
                nv12_stride = n.stride,
                "NV12 frames, converted to XRGB8888 on the CPU"
            );
        }
        cam.configure(&mut cfgs).map_err(at("configure"))?;
        // The configured mode's frame durations, where the pipeline
        // reports them: its shortest says how fast it can run.
        let max_fps = range(
            cam.controls(),
            <controls::FrameDurationLimits as ControlEntry>::ID,
        )
        .map(|(min_us, _)| min_us)
        .filter(|&us| us > 0.0)
        .map(|us| 1e6 / us);
        tracing::info!(generation, camera, ?max_fps, "frame rate");
        let stream = cfgs
            .get(0)
            .and_then(|sc| sc.stream())
            .ok_or_else(|| at("stream")(io::Error::other("no stream after configure")))?;
        let raw = match raw_layout {
            Some((size, stride, first_red)) => Some(RawStream {
                stream: cfgs.get(1).and_then(|sc| sc.stream()).ok_or_else(|| {
                    at("raw stream")(io::Error::other("no raw stream after configure"))
                })?,
                size,
                stride,
                first_red,
            }),
            None => None,
        };

        let mut alloc = FrameBufferAllocator::new(&cam);
        let fbs = alloc.alloc(&stream).map_err(at("allocate buffers"))?;
        let mut raw_fbs = match &raw {
            Some(r) => alloc
                .alloc(&r.stream)
                .map_err(at("allocate raw buffers"))?
                .into_iter()
                .map(Some)
                .collect(),
            None => Vec::new(),
        };
        tracing::info!(
            generation,
            count = fbs.len(),
            asked = count,
            frame_bytes = frame_size,
            total_mib = format!(
                "{:.1}",
                fbs.len() as f64 * frame_size as f64 / (1024.0 * 1024.0)
            ),
            "buffers allocated"
        );
        // With a raw stream, each request carries one buffer of each.
        let n = match &raw {
            Some(_) => fbs.len().min(raw_fbs.len()),
            None => fbs.len(),
        };
        let mut requests = Vec::with_capacity(n);
        for (i, fb) in fbs.into_iter().take(n).enumerate() {
            let map = |fb| {
                MappedBuffer::new(fb).map_err(|e| {
                    at("map buffer")(io::Error::other(format!("mapping a buffer: {e:?}")))
                })
            };
            let mut r = cam
                .create_request(Some(i as u64))
                .ok_or_else(|| at("create request")(io::Error::other("create_request failed")))?;
            r.add_buffer(&stream, map(fb)?).map_err(at("add buffer"))?;
            if let (Some(rs), Some(rfb)) = (&raw, raw_fbs.get_mut(i).and_then(Option::take)) {
                r.add_buffer(&rs.stream, map(rfb)?)
                    .map_err(at("add raw buffer"))?;
            }
            requests.push(r);
        }

        let me = self.me.clone();
        cam.on_request_completed(move |req| {
            let _ = me.send(Msg::Completed(req));
        });

        let awb_modes = cam
            .controls()
            .find(<controls::AwbMode as ControlEntry>::ID)
            .is_ok();
        let mut start_controls = ControlList::from_info_map(cam.controls()).unwrap_or_default();
        set_controls(
            &mut start_controls,
            controls,
            self.opts.exposure_hints,
            awb_modes,
        );
        cam.start(Some(&start_controls)).map_err(at("start"))?;

        let sensor_model = cam
            .properties()
            .get::<properties::Model>()
            .ok()
            .map(|m| m.0);
        let mut active = Active {
            cam,
            generation,
            camera_id: camera.to_string(),
            stream,
            size,
            stride,
            layout,
            copy: self.opts.display == DisplayMode::Memory
                || req.purpose == Purpose::Still
                || nv12.is_some(),
            nv12,
            awb_modes,
            lent: HashMap::new(),
            idle: Vec::new(),
            pending_controls: None,
            max_fps,
            raw,
            sensor_model,
            _alloc: alloc,
        };
        for r in requests {
            if let Err((r, e)) = active.cam.queue_request(r) {
                tracing::warn!(generation, error = %e, "queueing a request failed");
                active.idle.push(r);
            }
        }
        if !active.idle.is_empty() && active.idle.len() == count as usize {
            return Err(at("queue requests")(io::Error::other(
                "no request could be queued",
            )));
        }
        self.active = Some(active);
        Ok(())
    }

    fn requeue(&mut self, mut req: Request) {
        let hints = self.opts.exposure_hints;
        let Some(a) = self.active.as_mut() else {
            return;
        };
        req.reuse(ReuseFlag::REUSE_BUFFERS);
        if let Some(c) = a.pending_controls.take() {
            set_controls(req.controls_mut(), &c, hints, a.awb_modes);
        }
        if let Err((req, e)) = a.cam.queue_request(req) {
            tracing::warn!(error = %e, "requeue failed");
            a.idle.push(req);
        }
    }

    fn completed(&mut self, req: Request) {
        let Some(a) = self.active.as_mut() else {
            return; // a stopped stream's leftovers
        };
        if req.status() != RequestStatus::Complete {
            return; // canceled by a stop
        }
        let Some(fb) = req.buffer::<MappedBuffer>(&a.stream) else {
            return;
        };
        let ok = fb
            .metadata()
            .is_some_and(|m| m.status() == FrameMetadataStatus::Success);
        if !ok {
            self.requeue(req);
            return;
        }
        let meta = frame_meta(&req);
        let raw = a.raw.as_ref().and_then(|r| {
            let fb = req.buffer::<MappedBuffer>(&r.stream)?;
            let ok = fb
                .metadata()
                .is_some_and(|m| m.status() == FrameMetadataStatus::Success);
            if !ok {
                return None;
            }
            let planes = fb.data();
            let plane = planes.first()?;
            let len = (r.stride as usize * r.size.height as usize).min(plane.len());
            Some(Box::new(RawFrame {
                size: r.size,
                stride: r.stride,
                first_red: r.first_red,
                data: plane[..len].to_vec(),
                isp: isp_meta(&req, a.sensor_model.clone()),
            }))
        });
        let (generation, size, stride, layout, nv12) =
            (a.generation, a.size, a.stride, a.layout, a.nv12);
        let camera = a.camera_id.clone();
        let data = match a.copy {
            true => {
                let copy = {
                    let planes = fb.data();
                    match &nv12 {
                        Some(n) => nv12_to_xrgb(&planes, size, n),
                        None => planes.first().map(|plane| {
                            let len = (stride as usize * size.height as usize).min(plane.len());
                            plane[..len].to_vec()
                        }),
                    }
                };
                self.requeue(req);
                match copy {
                    Some(c) => FrameData::Memory(c),
                    None => return,
                }
            }
            false => {
                let plane = {
                    let planes = fb.planes();
                    planes
                        .get(0)
                        .map(|p| (p.fd(), p.offset().unwrap_or(0) as u32))
                };
                let Some((fd, offset)) = plane else {
                    self.requeue(req);
                    return;
                };
                let cookie = req.cookie();
                let me = self.me.clone();
                let release = ReleaseToken::new(move || {
                    let _ = me.send(Msg::Release { generation, cookie });
                });
                a.lent.insert(cookie, req);
                FrameData::Dmabuf {
                    fd,
                    offset,
                    release,
                }
            }
        };
        (self.sink)(Event::Frame(Frame {
            generation,
            camera,
            size,
            stride,
            layout,
            data,
            meta,
            raw,
        }));
    }

    fn release(&mut self, generation: u64, cookie: u64) {
        let Some(a) = self.active.as_mut().filter(|a| a.generation == generation) else {
            return;
        };
        if let Some(req) = a.lent.remove(&cookie) {
            self.requeue(req);
        }
    }

    /// Stop and drop the active camera, if any, and say so.
    fn stop(&mut self, rx: &mpsc::Receiver<Msg>) {
        let Some(mut a) = self.active.take() else {
            return;
        };
        // Give the UI a moment to hand back lent buffers: their fds close
        // with the allocator.
        let deadline = Instant::now() + LENT_WAIT;
        while !a.lent.is_empty() && Instant::now() < deadline {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Msg::Release { generation, cookie }) if generation == a.generation => {
                    a.lent.remove(&cookie);
                }
                Ok(Msg::Completed(_)) | Ok(Msg::Release { .. }) => {}
                Ok(other) => self.deferred.push_back(other),
                Err(_) => break,
            }
        }
        if !a.lent.is_empty() {
            tracing::warn!(count = a.lent.len(), "stopping with buffers still lent");
        }
        if let Err(e) = a.cam.stop() {
            tracing::warn!(error = %e, "stop failed");
        }
        // stop() completed every queued request as canceled: collect them
        // so they're dropped before the camera and its allocator.
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Completed(_) | Msg::Release { .. } => {}
                other => self.deferred.push_back(other),
            }
        }
        let generation = a.generation;
        drop(a);
        (self.sink)(Event::Stopped { generation });
    }
}

/// An NV12 frame as XRGB8888 (B, G, R, X in memory), `size.width * 4`
/// bytes a line. The planes are copied out first in one go each (the
/// camera's buffers may be mapped uncached), then converted on a few
/// threads. None when the planes are shorter than the frame needs.
fn nv12_to_xrgb(planes: &[&[u8]], size: Size, n: &Nv12) -> Option<Vec<u8>> {
    let (width, height, stride) = (size.width as usize, size.height as usize, n.stride as usize);
    let chroma_width = width.div_ceil(2) * 2;
    if width == 0 || height == 0 || stride < chroma_width {
        return None;
    }
    let chroma_rows = height.div_ceil(2);
    let y_len = stride * (height - 1) + width;
    let uv_len = stride * (chroma_rows - 1) + chroma_width;
    // Two planes, or one with the CbCr lines after the Y lines.
    let (y, uv) = match planes {
        [y, uv, ..] => (y.get(..y_len)?.to_vec(), uv.get(..uv_len)?.to_vec()),
        [one] => {
            let uv_start = stride * height;
            (
                one.get(..y_len)?.to_vec(),
                one.get(uv_start..uv_start + uv_len)?.to_vec(),
            )
        }
        [] => return None,
    };
    let line = width * 4;
    let mut out = vec![0u8; line * height];
    let threads = std::thread::available_parallelism()
        .map_or(1, |t| t.get())
        .clamp(1, 4);
    // Whole pairs of lines per thread: each pair shares one CbCr line.
    let rows = height.div_ceil(threads).div_ceil(2) * 2;
    std::thread::scope(|scope| {
        for (i, chunk) in out.chunks_mut(rows * line).enumerate() {
            let (y, uv) = (&y, &uv);
            scope.spawn(move || nv12_rows(y, uv, chunk, width, stride, i * rows, n));
        }
    });
    Some(out)
}

/// Convert the lines of `out` (from line `first` of the frame on).
fn nv12_rows(
    y: &[u8],
    uv: &[u8],
    out: &mut [u8],
    width: usize,
    stride: usize,
    first: usize,
    n: &Nv12,
) {
    const HALF: i32 = 1 << 15;
    let clamp = |v: i32| (v >> 16).clamp(0, 255) as u8;
    for (r, dst) in out.chunks_exact_mut(width * 4).enumerate() {
        let row = first + r;
        let luma = &y[row * stride..row * stride + width];
        let chroma = &uv[(row / 2) * stride..];
        for (x, px) in dst.chunks_exact_mut(4).enumerate() {
            let l = (luma[x] as i32 - n.y_offset) * n.y_scale + HALF;
            let c = x & !1;
            let u = chroma[c] as i32 - 128;
            let v = chroma[c + 1] as i32 - 128;
            px[0] = clamp(l + n.bu * u);
            px[1] = clamp(l - n.gu * u - n.gv * v);
            px[2] = clamp(l + n.rv * v);
            px[3] = 255;
        }
    }
}

/// ColourGains is an array control: its range is per element.
fn color_gains_range(map: &ControlInfoMap) -> Option<(f32, f32)> {
    let info = map.find(<controls::ColourGains as ControlEntry>::ID).ok()?;
    let first = |v: ControlValue| match v {
        ControlValue::Float(x) => x.first().copied(),
        _ => None,
    };
    let (lo, hi) = (first(info.min())?, first(info.max())?);
    (hi >= lo).then_some((lo, hi))
}

fn number(v: ControlValue) -> Option<f64> {
    match v {
        ControlValue::Float(x) => x.first().map(|v| *v as f64),
        ControlValue::Int32(x) => x.first().map(|v| *v as f64),
        ControlValue::Int64(x) => x.first().map(|v| *v as f64),
        ControlValue::Uint32(x) => x.first().map(|v| *v as f64),
        _ => None,
    }
}

/// A control's range, when the map lists it with numeric bounds.
fn range(map: &ControlInfoMap, id: u32) -> Option<(f64, f64)> {
    let info = map.find(id).ok()?;
    let (lo, hi) = (number(info.min())?, number(info.max())?);
    (hi >= lo).then_some((lo, hi))
}

/// What the camera offers for manual exposure, white balance and focus.
/// Everything is looked up by control id, so a libcamera without one of
/// them just leaves that control disabled in the app.
fn caps(map: &ControlInfoMap) -> Caps {
    let has = |id: u32| map.find(id).is_ok();
    let id = |c: u32| c;
    Caps {
        ev: range(map, id(<controls::ExposureValue as ControlEntry>::ID))
            .map(|(a, b)| (a as f32, b as f32)),
        exposure_time: range(map, <controls::ExposureTime as ControlEntry>::ID)
            .map(|(a, b)| (a as i32, b as i32)),
        gain: range(map, <controls::AnalogueGain as ControlEntry>::ID)
            .map(|(a, b)| (a as f32, b as f32)),
        manual_exposure: has(<controls::ExposureTimeMode as ControlEntry>::ID)
            || has(<controls::AeEnable as ControlEntry>::ID),
        awb_modes: has(<controls::AwbMode as ControlEntry>::ID),
        color_temperature: range(map, <controls::ColourTemperature as ControlEntry>::ID)
            .filter(|_| has(<controls::AwbEnable as ControlEntry>::ID))
            .map(|(a, b)| (a as i32, b as i32)),
        lens_position: range(map, <controls::LensPosition as ControlEntry>::ID)
            .filter(|_| has(<controls::AfMode as ControlEntry>::ID))
            .map(|(a, b)| (a as f32, b as f32)),
        ae_enable: has(<controls::AeEnable as ControlEntry>::ID),
        ae_metering: has(<controls::AeMeteringMode as ControlEntry>::ID)
            && has(<controls::AfWindows as ControlEntry>::ID),
        af_pause: has(<controls::AfPause as ControlEntry>::ID),
        color_gains: color_gains_range(map)
            .filter(|_| has(<controls::AwbEnable as ControlEntry>::ID)),
    }
}

/// Exposure, white balance and manual focus on a control list. Modes go
/// with values: a manual exposure time is ExposureTimeMode Manual plus
/// ExposureTime (or AeEnable off on a libcamera without the modes), and
/// Auto puts the mode back.
fn set_settings(list: &mut ControlList, s: &ExposureSettings, awb_modes: bool) {
    let _ = list.set(controls::ExposureValue(s.ev));
    if s.ae_hold {
        // AE lock: the camera holds what it has. Mode controls in the same
        // request would win over it, so none are sent.
        let _ = list.set(controls::AeEnable(false));
    } else {
        match s.shutter {
            Setting::Manual(t) => {
                let _ = list.set(controls::ExposureTimeMode::Manual);
                let _ = list.set(controls::ExposureTime(t));
            }
            Setting::Auto => {
                let _ = list.set(controls::ExposureTimeMode::Auto);
            }
        }
        match s.gain {
            Setting::Manual(g) => {
                let _ = list.set(controls::AnalogueGainMode::Manual);
                let _ = list.set(controls::AnalogueGain(g));
            }
            Setting::Auto => {
                let _ = list.set(controls::AnalogueGainMode::Auto);
            }
        }
        // The older switch, for a libcamera without the mode controls.
        let _ = list.set(controls::AeEnable(s.shutter.is_auto() && s.gain.is_auto()));
    }
    let _ = list.set(if s.spot {
        controls::AeMeteringMode::MeteringSpot
    } else {
        controls::AeMeteringMode::MeteringMatrix
    });
    match s.white_balance {
        Setting::Auto => {
            let _ = list.set(controls::AwbEnable(true));
            if awb_modes {
                let _ = list.set(controls::AwbMode::AwbAuto);
            }
        }
        Setting::Manual(Awb::Preset(p)) => {
            let _ = list.set(controls::AwbEnable(true));
            let _ = list.set(match p {
                AwbPreset::Daylight => controls::AwbMode::AwbDaylight,
                AwbPreset::Cloudy => controls::AwbMode::AwbCloudy,
                AwbPreset::Incandescent => controls::AwbMode::AwbIncandescent,
                AwbPreset::Fluorescent => controls::AwbMode::AwbFluorescent,
            });
        }
        Setting::Manual(Awb::Kelvin(k)) => {
            let _ = list.set(controls::AwbEnable(false));
            let _ = list.set(controls::ColourTemperature(k as i32));
        }
        Setting::Manual(Awb::Gains(g)) => {
            let _ = list.set(controls::AwbEnable(false));
            let _ = list.set(controls::ColourGains(g));
        }
    }
    match s.lens {
        Setting::Manual(p) => {
            let _ = list.set(controls::AfMode::Manual);
            let _ = list.set(controls::LensPosition(p));
        }
        Setting::Auto => {
            let _ = list.set(if s.af_hold {
                controls::AfPause::Immediate
            } else {
                controls::AfPause::Resume
            });
        }
    }
}

fn set_window(list: &mut ControlList, w: &Rect) {
    let _ = list.set(controls::AfMetering::Windows);
    let _ = list.set(controls::AfWindows(vec![Rectangle {
        x: w.x,
        y: w.y,
        width: w.width,
        height: w.height,
    }]));
}

/// Put `c` on a control list. A control the camera doesn't have fails to
/// set; that's fine.
fn set_controls(list: &mut ControlList, c: &Controls, exposure_hints: bool, awb_modes: bool) {
    if c.af_continuous {
        let _ = list.set(controls::AfMode::Continuous);
    }
    if let Some(s) = &c.settings {
        set_settings(list, s, awb_modes);
    }
    if let Some(w) = &c.metering_window {
        set_window(list, w);
    }
    if let Some(w) = &c.af_window {
        // Continuous AF follows this window from now on.
        let _ = list.set(controls::AfMode::Continuous);
        set_window(list, w);
    }
    if let Some(d) = c.frame_duration_us {
        let _ = list.set(controls::FrameDurationLimits([d, d]));
    }
    if let Some(focus) = c.focus {
        // One scan: auto mode, the tapped window when the camera takes
        // windows, then the trigger, all in the same request.
        let _ = list.set(controls::AfMode::Auto);
        if let Some(w) = focus.window {
            set_window(list, &w);
        }
        let _ = list.set(controls::AfTrigger::Start);
    }
    if exposure_hints {
        if let Some(t) = c.exposure_us {
            let _ = list.set(controls::ExposureTime(t));
        }
        if let Some(g) = c.analog_gain {
            let _ = list.set(controls::AnalogueGain(g));
        }
    }
}

/// The raw stream's size, stride and Bayer phase, if the validated
/// configuration has one at the processed stream's size in a format the
/// raw merge reads.
/// How much wider than the processed frame a raw frame may be.
const RAW_EXTRA_COLUMNS: u32 = 16;

fn raw_layout(cfgs: &libcamera::camera::CameraConfiguration) -> Option<(Size, u32, (u8, u8))> {
    let (processed, raw) = (cfgs.get(0)?, cfgs.get(1)?);
    let fmt = raw.get_pixel_format();
    let first_red = RAW_FORMATS
        .iter()
        .find(|(f, _)| *f == fmt)
        .map(|(_, r)| *r)?;
    let (ps, rs) = (processed.get_size(), raw.get_size());
    // The software ISP's output can be a few columns narrower than the
    // sensor's (4072 of 4080 on the S5KGN1): the raw frame's left part.
    (ps.height == rs.height && (ps.width..=ps.width + RAW_EXTRA_COLUMNS).contains(&rs.width))
        .then(|| (Size::new(rs.width, rs.height), raw.get_stride(), first_red))
}

/// What the ISP used for this frame, from its metadata.
fn isp_meta(req: &Request, sensor_model: Option<String>) -> IspMeta {
    let md = req.metadata();
    IspMeta {
        sensor_model,
        black_level: md
            .get::<controls::SensorBlackLevels>()
            .ok()
            .map(|b| b.0.iter().map(|&v| v as f32).sum::<f32>() / 4.0),
        color_matrix: md
            .get::<controls::ColourCorrectionMatrix>()
            .ok()
            .map(|m| m.0),
        saturation: md.get::<controls::Saturation>().ok().map(|v| v.0),
        contrast: md.get::<controls::Contrast>().ok().map(|v| v.0),
        sharpness: md.get::<controls::Sharpness>().ok().map(|v| v.0),
        gamma: md.get::<controls::Gamma>().ok().map(|v| v.0),
    }
}

fn frame_meta(req: &Request) -> FrameMeta {
    let md = req.metadata();
    FrameMeta {
        sequence: req.sequence(),
        timestamp_ns: md
            .get::<controls::SensorTimestamp>()
            .map(|t| t.0 as u64)
            .unwrap_or(0),
        exposure_us: md.get::<controls::ExposureTime>().ok().map(|v| v.0),
        analog_gain: md.get::<controls::AnalogueGain>().ok().map(|v| v.0),
        digital_gain: md.get::<controls::DigitalGain>().ok().map(|v| v.0),
        color_gains: md.get::<controls::ColourGains>().ok().map(|v| v.0),
        af_state: md.get::<controls::AfState>().ok().map(|s| match s {
            controls::AfState::Idle => AfState::Idle,
            controls::AfState::Scanning => AfState::Scanning,
            controls::AfState::Focused => AfState::Focused,
            controls::AfState::Failed => AfState::Failed,
        }),
        lens_position: md.get::<controls::LensPosition>().ok().map(|v| v.0),
        ae_state: md.get::<controls::AeState>().ok().map(|s| match s {
            controls::AeState::Idle => AeState::Idle,
            controls::AeState::Searching => AeState::Searching,
            controls::AeState::Converged => AeState::Converged,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_range() -> Nv12 {
        Nv12::new(8, None)
    }

    #[test]
    fn nv12_gray_stays_gray_and_colors_land_in_their_channel() {
        // 4x2 frame, stride 8: Y lines, then one CbCr line.
        let mut frame = vec![0u8; 8 * 3];
        frame[..4].copy_from_slice(&[0, 128, 255, 128]);
        frame[8..12].copy_from_slice(&[128, 128, 128, 128]);
        // Pixels 0-1 gray (Cb = Cr = 128), pixels 2-3 red (Cr high).
        frame[16..20].copy_from_slice(&[128, 128, 128, 255]);
        let out = nv12_to_xrgb(&[&frame], Size::new(4, 2), &full_range()).expect("converted");
        assert_eq!(out.len(), 4 * 4 * 2);
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..8], &[128, 128, 128, 255]);
        // Y 255 with Cr 255: red saturates, blue stays at Y.
        assert_eq!(out[8 + 2], 255);
        assert_eq!(out[8], 255);
        // Y 128 with Cr 255: more red than green and blue.
        assert!(out[12 + 2] > 200 && out[12 + 1] < 128 && out[12] == 128);
    }

    #[test]
    fn nv12_two_planes_and_short_planes() {
        let y = vec![200u8; 8 * 4];
        let uv = vec![128u8; 8 * 2];
        let out = nv12_to_xrgb(&[&y, &uv], Size::new(6, 4), &full_range()).expect("converted");
        assert!(out.chunks_exact(4).all(|p| p == [200, 200, 200, 255]));
        assert!(nv12_to_xrgb(&[&y[..10], &uv], Size::new(6, 4), &full_range()).is_none());
        assert!(nv12_to_xrgb(&[], Size::new(6, 4), &full_range()).is_none());
    }

    #[test]
    fn nv12_limited_range_stretches_to_full() {
        let n = Nv12::new(
            2,
            Some(ColorSpace::new(
                libcamera::color_space::Primaries::Rec709,
                libcamera::color_space::TransferFunction::Rec709,
                YcbcrEncoding::Rec709,
                Range::Limited,
            )),
        );
        let frame = [16u8, 235, 16, 235, 128, 128];
        let out = nv12_to_xrgb(&[&frame], Size::new(2, 2), &n).expect("converted");
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..8], &[255, 255, 255, 255]);
    }
}
