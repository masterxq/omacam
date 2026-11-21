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
//! A camera that gives YUYV or NV12 instead of the XRGB8888 asked for (a
//! hardware ISP with YUV output, such as the MediaTek DIP) has its frames
//! copied out as they are, in either display mode: the viewfinder and the
//! recorder take YUV, and photos turn it into RGB off this thread.
//!
//! A preview that may take stills asks for a still stream beside it (the
//! viewfinder and still roles in one configuration). Where the camera
//! validates the pair, a still is one buffer of that stream added to the
//! next request: the preview runs on, at its settled exposure and focus.
//! Where it doesn't, the preview starts alone, as before.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::ptr::NonNull;
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
    Purpose, RawFrame, Rect, ReleaseToken, Setting, Size, StreamRequest, YuvColor,
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

/// A buffer as a request holds it: a pointer to one of [`Active`]'s
/// mapped buffers, and its index there. Requests hold these rather than
/// the buffers themselves so that a request that carried a still can let
/// go of the still's buffer (`reuse` without its buffers) and take its
/// preview buffer back, which the buffers' owner keeps meanwhile.
struct BufRef {
    ptr: NonNull<libcamera_sys::libcamera_framebuffer_t>,
    index: usize,
}

impl BufRef {
    fn of(buffers: &[MappedBuffer], index: usize) -> BufRef {
        BufRef {
            // SAFETY: the buffer is a valid libcamera FrameBuffer from the
            // allocator (MemoryMappedFrameBuffer only maps it).
            ptr: unsafe { buffers[index].ptr() },
            index,
        }
    }
}

// SAFETY: a pointer to a FrameBuffer that `Active` owns and outlives every
// request (it drops the camera, which completes and returns them, first);
// libcamera reads it only on the camera's thread and its own.
unsafe impl Send for BufRef {}

impl AsFrameBuffer for BufRef {
    unsafe fn ptr(&self) -> NonNull<libcamera_sys::libcamera_framebuffer_t> {
        self.ptr
    }
}

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
    /// Copy frames out (memory display, stills, which the encoder reads on
    /// the CPU, and YUV frames, which the viewfinder takes from memory).
    copy: bool,
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
    /// The still stream beside the preview, when the start asked for one
    /// and the camera runs both in one configuration.
    still: Option<StillStream>,
    /// The sensor's model, for the raw frames' tuning file.
    sensor_model: Option<String>,
    /// The processed stream's buffers; request `i` (its cookie) holds
    /// buffer `i`, and raw buffer `i` with a raw stream.
    buffers: Vec<MappedBuffer>,
    raw_buffers: Vec<MappedBuffer>,
    _alloc: FrameBufferAllocator,
}

struct RawStream {
    stream: Stream,
    size: Size,
    stride: u32,
    first_red: (u8, u8),
}

/// A still stream configured beside the preview. Its buffers ride along
/// with preview requests: one is added to the next request queued after a
/// capture is asked for.
struct StillStream {
    stream: Stream,
    size: Size,
    stride: u32,
    layout: PixelLayout,
    buffers: Vec<MappedBuffer>,
    /// Indices of the buffers no request holds.
    free: Vec<usize>,
    /// Stills asked for and not yet put on a request.
    wanted: u32,
}

/// The streams a start asks libcamera for, in the order tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// The preview and a still stream beside it.
    Beside,
    /// The processed stream and a raw one of the same size.
    Raw,
    /// One processed stream: the viewfinder, or the video role.
    Single,
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
                            still: a.still.as_ref().map(|s| s.size),
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
            Command::Capture { generation } => {
                match self
                    .active
                    .as_mut()
                    .filter(|a| a.generation == generation)
                    .and_then(|a| a.still.as_mut())
                {
                    Some(st) => st.wanted = 1,
                    None => tracing::warn!(generation, "a still asked of a stream without one"),
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
    /// Start `req`, trying again when memory runs short: once more after
    /// releasing the camera manager, then with one buffer, then at each
    /// fallback size. A still stream also falls back on other start
    /// errors, and a preview that failed with a still stream beside it
    /// tries again without one. Returns a note for the user when it had to
    /// settle for less.
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
        let mut beside = req.still.is_some();
        for (i, &(size, buffers)) in attempts.iter().enumerate() {
            let fallback = i > 0;
            if fallback {
                tracing::warn!(
                    generation,
                    camera,
                    size = %size.map(|s| s.to_string()).unwrap_or_else(|| "default".into()),
                    buffers,
                    beside,
                    "retrying the start"
                );
            }
            match self.start(generation, camera, size, buffers, req, controls, beside) {
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
                    // The pair may be what failed: the preview alone next.
                    let retry = e.is_out_of_memory() || req.purpose == Purpose::Still || beside;
                    beside = false;
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

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        generation: u64,
        camera: &str,
        size_wanted: Option<Size>,
        buffers: u32,
        req: &StreamRequest,
        controls: &Controls,
        beside: bool,
    ) -> Result<(), StartError> {
        let at = |step: &'static str| move |error: io::Error| StartError { step, error };
        self.open_manager().map_err(at("camera manager"))?;
        let mgr = self.manager.as_ref().expect("opened");
        let cam = mgr.get(camera).ok_or_else(|| {
            at("find camera")(io::Error::new(io::ErrorKind::NotFound, "no such camera"))
        })?;
        let mut cam: ActiveCamera<'static> = cam.acquire().map_err(at("acquire"))?;

        // What to try, best first: the preview with a still stream beside
        // it, or with a raw stream at its size, where asked for and the
        // camera gives them; the one processed stream in any case.
        let mut plans = Vec::new();
        if beside && req.purpose == Purpose::Preview && !req.raw {
            plans.push(Plan::Beside);
        }
        if req.raw {
            plans.push(Plan::Raw);
        }
        plans.push(Plan::Single);
        let main_role = if req.video {
            StreamRole::VideoRecording
        } else {
            StreamRole::ViewFinder
        };
        let mut chosen = None;
        for plan in plans {
            let roles: &[StreamRole] = match plan {
                Plan::Beside => &[StreamRole::ViewFinder, StreamRole::StillCapture],
                Plan::Raw => &[main_role, StreamRole::Raw],
                Plan::Single => &[main_role],
            };
            let Some(mut cfgs) = cam.generate_configuration(roles) else {
                tracing::info!(
                    generation,
                    camera,
                    ?plan,
                    "no configuration for these roles"
                );
                continue;
            };
            {
                let mut sc = cfgs.get_mut(0).expect("one stream");
                // XRGB8888 unless the role's own format is one omacam
                // takes as it is (the video role's YUYV): the safe GPU
                // import format, and what the software ISP gives.
                let own = layout_of(sc.get_pixel_format(), None).is_some_and(|l| l.yuv().is_some());
                if !(req.video && own) {
                    sc.set_pixel_format(XRGB8888);
                }
                if let Some(s) = size_wanted {
                    sc.set_size(LcSize::new(s.width, s.height));
                }
                sc.set_buffer_count(buffers.max(1));
            }
            if let Some(mut other) = cfgs.get_mut(1) {
                match plan {
                    Plan::Raw => {
                        if let Some(s) = size_wanted {
                            other.set_size(LcSize::new(s.width, s.height));
                        }
                        other.set_buffer_count(buffers.max(1));
                    }
                    Plan::Beside => {
                        // The still in the camera's own format where
                        // omacam reads it, else XRGB8888.
                        if layout_of(other.get_pixel_format(), None).is_none() {
                            other.set_pixel_format(XRGB8888);
                        }
                        if let Some(s) = req.still {
                            other.set_size(LcSize::new(s.width, s.height));
                        }
                        other.set_buffer_count(req.still_buffers.max(1));
                    }
                    Plan::Single => {}
                }
            }
            let status = cfgs.validate();
            tracing::info!(
                generation,
                camera,
                purpose = ?req.purpose,
                ?plan,
                video = req.video,
                asked = %size_wanted.map(|s| s.to_string()).unwrap_or_else(|| "default".into()),
                buffers,
                status = ?status,
                config = %cfgs.to_string_repr(),
                "configuration validated"
            );
            if status.is_invalid() {
                if plan == Plan::Single {
                    return Err(at("validate")(io::Error::from_raw_os_error(EINVAL)));
                }
                continue;
            }
            let usable = match plan {
                Plan::Single => true,
                Plan::Raw => raw_layout(&cfgs).is_some(),
                Plan::Beside => beside_usable(&cfgs),
            };
            if !usable {
                tracing::info!(
                    generation,
                    camera,
                    ?plan,
                    "the camera adjusted the streams past use: trying fewer"
                );
                if plan == Plan::Raw {
                    tracing::warn!(
                        generation,
                        camera,
                        "no raw stream to match the processed one: bursts merge the processed frames"
                    );
                }
                continue;
            }
            chosen = Some((cfgs, plan));
            break;
        }
        let Some((mut cfgs, plan)) = chosen else {
            return Err(at("generate configuration")(io::Error::other(
                "no configuration",
            )));
        };
        let (layout, size, stride, frame_size, count) = {
            let sc = cfgs.get(0).expect("one stream");
            let fmt = sc.get_pixel_format();
            let s = sc.get_size();
            let layout = layout_of(fmt, sc.get_color_space()).ok_or_else(|| {
                at("format")(io::Error::other(format!(
                    "unsupported pixel format {:#x}",
                    fmt.fourcc()
                )))
            })?;
            (
                layout,
                Size::new(s.width, s.height),
                sc.get_stride(),
                sc.get_frame_size(),
                sc.get_buffer_count(),
            )
        };
        let raw_shape = match plan {
            Plan::Raw => raw_layout(&cfgs),
            _ => None,
        };
        let still_shape = match plan {
            Plan::Beside => {
                let sc = cfgs.get(1).expect("a still stream");
                let s = sc.get_size();
                layout_of(sc.get_pixel_format(), sc.get_color_space())
                    .map(|l| (Size::new(s.width, s.height), sc.get_stride(), l))
            }
            _ => None,
        };
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
        let second = || {
            cfgs.get(1)
                .and_then(|sc| sc.stream())
                .ok_or_else(|| at("second stream")(io::Error::other("no second stream")))
        };
        let raw = match raw_shape {
            Some((size, stride, first_red)) => Some(RawStream {
                stream: second()?,
                size,
                stride,
                first_red,
            }),
            None => None,
        };
        let still_stream = match still_shape {
            Some(shape) => Some((second()?, shape)),
            None => None,
        };

        let map = |fb| {
            MappedBuffer::new(fb)
                .map_err(|e| at("map buffer")(io::Error::other(format!("mapping a buffer: {e:?}"))))
        };
        let mut alloc = FrameBufferAllocator::new(&cam);
        let buffers = alloc
            .alloc(&stream)
            .map_err(at("allocate buffers"))?
            .into_iter()
            .map(map)
            .collect::<Result<Vec<_>, _>>()?;
        let raw_buffers = match &raw {
            Some(r) => alloc
                .alloc(&r.stream)
                .map_err(at("allocate raw buffers"))?
                .into_iter()
                .map(map)
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        let still = match still_stream {
            Some((stream, (size, stride, layout))) => {
                let buffers = alloc
                    .alloc(&stream)
                    .map_err(at("allocate still buffers"))?
                    .into_iter()
                    .map(map)
                    .collect::<Result<Vec<_>, _>>()?;
                tracing::info!(
                    generation,
                    %size,
                    stride,
                    ?layout,
                    count = buffers.len(),
                    "still stream beside the preview"
                );
                Some(StillStream {
                    stream,
                    size,
                    stride,
                    layout,
                    free: (0..buffers.len()).rev().collect(),
                    buffers,
                    wanted: 0,
                })
            }
            None => None,
        };
        tracing::info!(
            generation,
            count = buffers.len(),
            asked = count,
            frame_bytes = frame_size,
            ?layout,
            total_mib = format!(
                "{:.1}",
                buffers.len() as f64 * frame_size as f64 / (1024.0 * 1024.0)
            ),
            "buffers allocated"
        );
        // With a raw stream, each request carries one buffer of each.
        let n = match &raw {
            Some(_) => buffers.len().min(raw_buffers.len()),
            None => buffers.len(),
        };
        let mut requests = Vec::with_capacity(n);
        for i in 0..n {
            let mut r = cam
                .create_request(Some(i as u64))
                .ok_or_else(|| at("create request")(io::Error::other("create_request failed")))?;
            r.add_buffer(&stream, BufRef::of(&buffers, i))
                .map_err(at("add buffer"))?;
            if let Some(rs) = &raw {
                r.add_buffer(&rs.stream, BufRef::of(&raw_buffers, i))
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
                || layout.yuv().is_some(),
            awb_modes,
            lent: HashMap::new(),
            idle: Vec::new(),
            pending_controls: None,
            max_fps,
            raw,
            still,
            sensor_model,
            buffers,
            raw_buffers,
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
        let carried_still = a
            .still
            .as_ref()
            .is_some_and(|s| req.find_buffer(&s.stream).is_some());
        if carried_still {
            // Let go of the still's buffer (the completion handler took
            // it back) and put the request's own buffers back on.
            req.reuse(ReuseFlag::empty());
            let i = req.cookie() as usize;
            if let Err(e) = req.add_buffer(&a.stream, BufRef::of(&a.buffers, i)) {
                tracing::warn!(error = %e, "a preview buffer didn't go back on its request");
                return;
            }
            if let Some(r) = &a.raw {
                if let Err(e) = req.add_buffer(&r.stream, BufRef::of(&a.raw_buffers, i)) {
                    tracing::warn!(error = %e, "a raw buffer didn't go back on its request");
                    return;
                }
            }
        } else {
            req.reuse(ReuseFlag::REUSE_BUFFERS);
        }
        if let Some(st) = a.still.as_mut().filter(|s| s.wanted > 0) {
            if let Some(i) = st.free.pop() {
                match req.add_buffer(&st.stream, BufRef::of(&st.buffers, i)) {
                    Ok(()) => {
                        st.wanted -= 1;
                        tracing::debug!(sequence = req.sequence(), "still asked for");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "the still's buffer didn't go on a request");
                        st.free.push(i);
                    }
                }
            }
        }
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
        let meta = frame_meta(&req);
        // A still's buffer: copied out after the preview went on.
        let still = a
            .still
            .as_ref()
            .and_then(|s| req.buffer::<BufRef>(&s.stream))
            .map(|b| b.index);
        let (generation, size, stride, layout) = (a.generation, a.size, a.stride, a.layout);
        let camera = a.camera_id.clone();
        let preview = req.buffer::<BufRef>(&a.stream).map(|b| b.index);
        let ok = preview.is_some_and(|i| {
            a.buffers[i]
                .metadata()
                .is_some_and(|m| m.status() == FrameMetadataStatus::Success)
        });
        match preview.filter(|_| ok) {
            None => self.requeue(req),
            Some(i) => {
                let raw = a.raw.as_ref().and_then(|r| {
                    let fb = &a.raw_buffers[req.buffer::<BufRef>(&r.stream)?.index];
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
                let data = if a.copy {
                    let copy = copy_frame(&a.buffers[i].data(), layout, stride, size.height);
                    self.requeue(req);
                    copy.map(FrameData::Memory)
                } else {
                    let plane = {
                        let planes = a.buffers[i].planes();
                        planes
                            .get(0)
                            .map(|p| (p.fd(), p.offset().unwrap_or(0) as u32))
                    };
                    match plane {
                        Some((fd, offset)) => {
                            let cookie = req.cookie();
                            let me = self.me.clone();
                            let release = ReleaseToken::new(move || {
                                let _ = me.send(Msg::Release { generation, cookie });
                            });
                            a.lent.insert(cookie, req);
                            Some(FrameData::Dmabuf {
                                fd,
                                offset,
                                release,
                            })
                        }
                        None => {
                            self.requeue(req);
                            None
                        }
                    }
                };
                if let Some(data) = data {
                    (self.sink)(Event::Frame(Frame {
                        generation,
                        camera: camera.clone(),
                        size,
                        stride,
                        layout,
                        data,
                        meta: meta.clone(),
                        raw,
                        still: false,
                    }));
                }
            }
        }
        if let Some(i) = still {
            self.still_completed(i, generation, camera, meta);
        }
    }

    /// The still stream's buffer `i` came back: send its frame and free it.
    fn still_completed(&mut self, i: usize, generation: u64, camera: String, meta: FrameMeta) {
        let Some(st) = self.active.as_mut().and_then(|a| a.still.as_mut()) else {
            return;
        };
        let fb = &st.buffers[i];
        let ok = fb
            .metadata()
            .is_some_and(|m| m.status() == FrameMetadataStatus::Success);
        let copy = ok
            .then(|| copy_frame(&fb.data(), st.layout, st.stride, st.size.height))
            .flatten();
        st.free.push(i);
        let (size, stride, layout) = (st.size, st.stride, st.layout);
        match copy {
            Some(data) => (self.sink)(Event::Frame(Frame {
                generation,
                camera,
                size,
                stride,
                layout,
                data: FrameData::Memory(data),
                meta,
                raw: None,
                still: true,
            })),
            None => tracing::warn!(
                generation,
                ok,
                "a still frame came back without its picture"
            ),
        }
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

/// The layout of a stream format omacam reads: the RGB formats, YUYV and
/// NV12 (a hardware ISP with YUV output, such as the MediaTek DIP), the YUV
/// ones with their color space's matrix and range; BT.601 full range (as
/// JPEG) where the pipeline names none.
fn layout_of(fmt: PixelFormat, cs: Option<ColorSpace>) -> Option<PixelLayout> {
    if fmt.modifier() != 0 {
        return None;
    }
    let yuv = || YuvColor {
        bt709: cs.is_some_and(|c| c.ycbcr_encoding == YcbcrEncoding::Rec709),
        limited: cs.is_some_and(|c| c.range == Range::Limited),
    };
    if fmt == libcamera::formats::YUYV {
        Some(PixelLayout::Yuyv(yuv()))
    } else if fmt == libcamera::formats::NV12 {
        Some(PixelLayout::Nv12(yuv()))
    } else {
        PixelLayout::from_fourcc(fmt.fourcc())
    }
}

/// A validated preview + still pair omacam can use: both streams kept, in
/// formats it reads, and the still larger than the preview (else a photo
/// gains nothing from it).
fn beside_usable(cfgs: &libcamera::camera::CameraConfiguration) -> bool {
    if cfgs.len() != 2 {
        return false;
    }
    let (Some(p), Some(s)) = (cfgs.get(0), cfgs.get(1)) else {
        return false;
    };
    let pixels = |z: LcSize| z.width as u64 * z.height as u64;
    layout_of(p.get_pixel_format(), None).is_some()
        && layout_of(s.get_pixel_format(), None).is_some()
        && pixels(s.get_size()) > pixels(p.get_size())
}

/// One frame copied out of its buffer's planes into one piece: the plane
/// for the one-plane layouts, the Y lines then the CbCr lines for NV12
/// (from two planes, or one with both). The camera's buffers may be mapped
/// uncached, so each plane is read once, in one go. None when the planes
/// are shorter than the frame needs.
fn copy_frame(planes: &[&[u8]], layout: PixelLayout, stride: u32, height: u32) -> Option<Vec<u8>> {
    let len = layout.frame_len(stride, height);
    match (layout, planes) {
        (PixelLayout::Nv12(_), [y, uv, ..]) => {
            let y_len = stride as usize * height as usize;
            let mut out = Vec::with_capacity(len);
            out.extend_from_slice(y.get(..y_len)?);
            out.extend_from_slice(uv.get(..len - y_len)?);
            Some(out)
        }
        (PixelLayout::Nv12(_), [one]) => one.get(..len).map(<[u8]>::to_vec),
        // The last line may end without its padding.
        (_, [one, ..]) => Some(one[..len.min(one.len())].to_vec()),
        (_, []) => None,
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
    use omacam_core::backend::YuvColor;

    #[test]
    fn nv12_planes_are_copied_into_one_piece() {
        let nv12 = PixelLayout::Nv12(YuvColor::default());
        let y = vec![1u8; 8 * 4];
        let uv = vec![2u8; 8 * 2];
        let out = copy_frame(&[&y, &uv], nv12, 8, 4).expect("copied");
        assert_eq!(out.len(), 48);
        assert!(out[..32].iter().all(|&v| v == 1) && out[32..].iter().all(|&v| v == 2));
        // One plane with both, as long as the frame.
        let one: Vec<u8> = y.iter().chain(&uv).copied().collect();
        assert_eq!(copy_frame(&[&one], nv12, 8, 4), Some(out));
        // Short planes.
        assert!(copy_frame(&[&y[..30], &uv], nv12, 8, 4).is_none());
        assert!(copy_frame(&[&one[..40]], nv12, 8, 4).is_none());
        assert!(copy_frame(&[], nv12, 8, 4).is_none());
    }

    #[test]
    fn one_plane_frames_take_their_lines() {
        let yuyv = PixelLayout::Yuyv(YuvColor::default());
        let plane = vec![7u8; 100];
        assert_eq!(
            copy_frame(&[&plane], yuyv, 16, 4).map(|v| v.len()),
            Some(64)
        );
        // A last line without its padding is kept as it is.
        assert_eq!(
            copy_frame(&[&plane[..60]], PixelLayout::Xrgb8888, 16, 4).map(|v| v.len()),
            Some(60)
        );
    }

    #[test]
    fn yuv_formats_carry_their_color_space() {
        let rec709 = ColorSpace::new(
            libcamera::color_space::Primaries::Rec709,
            libcamera::color_space::TransferFunction::Rec709,
            YcbcrEncoding::Rec709,
            Range::Limited,
        );
        assert_eq!(
            layout_of(libcamera::formats::NV12, Some(rec709)),
            Some(PixelLayout::Nv12(YuvColor {
                bt709: true,
                limited: true
            }))
        );
        assert_eq!(
            layout_of(libcamera::formats::YUYV, None),
            Some(PixelLayout::Yuyv(YuvColor::default()))
        );
        assert_eq!(layout_of(XRGB8888, None), Some(PixelLayout::Xrgb8888));
        assert_eq!(layout_of(libcamera::formats::SRGGB10_CSI2P, None), None);
    }
}
