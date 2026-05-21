// handsets-viewer — live device-screen window backed by the handsets daemon.
//
// Codec modes:
//   --codec jpeg      (default): length-prefixed JPEG frames, zune-jpeg → BGRA.
//   --codec tilejpeg          : tiled JPEG deltas patched into a framebuffer.
//   --codec h264              : length-prefixed Annex-B H.264 access units;
//                               on macOS, VideoToolbox decodes straight into
//                               an IOSurface-backed CVPixelBuffer which the
//                               Metal renderer wraps as an MTLTexture and
//                               blits to the drawable (zero-copy GPU path).
//
// UI thread (winit 0.30) renders the most recent frame via the Metal-backed
// renderer. JPEG / tilejpeg take the CPU upload path; H.264 takes the
// zero-copy IOSurface path.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod metal_renderer;
#[cfg(target_os = "macos")]
mod videotoolbox_decoder;
#[cfg(target_os = "macos")]
use metal_renderer::MetalRenderer;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

const DAEMON_ADDR: &str = "127.0.0.1:9008";

// Default window: native aspect (1440x3120) at 1/4 scale.
const INITIAL_W: u32 = 360;
const INITIAL_H: u32 = 780;

const USAGE: &str = "\
Usage: handsets-viewer [options]

Options:
  --codec jpeg|h264|tilejpeg   default jpeg
  --size N            long-edge in px (default native; capped by encoder)
  --quality N         JPEG quality 1..100 (jpeg/tilejpeg, default 80)
  --bitrate-kbps N    target bitrate in kbps (h264 only, default 6000)
  --tile N            tile edge in px (tilejpeg only, default 128)
  --fps N             frame-rate hint
  --no-native         don't request max=1; use --size literally
  --host H            default 127.0.0.1
  --port P            default 9008
  -h, --help          show this message
";

#[derive(Debug, Clone)]
struct Args {
    codec: Codec,
    size: Option<u32>,
    quality: Option<u32>,
    bitrate_kbps: u32,
    fps: Option<u32>,
    tile: Option<u32>,
    native: bool,
    host: String,
    port: u16,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            codec: Codec::Jpeg,
            size: None,
            quality: None,
            bitrate_kbps: 6000,
            fps: None,
            tile: None,
            native: true,
            host: "127.0.0.1".into(),
            port: 9008,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Jpeg,
    H264,
    TileJpeg,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--codec" => {
                let v = it.next().ok_or("--codec needs a value")?;
                a.codec = match v.as_str() {
                    "jpeg" => Codec::Jpeg,
                    "h264" => Codec::H264,
                    "tilejpeg" | "tile-jpeg" | "tile" => Codec::TileJpeg,
                    _ => return Err(format!("unknown codec: {v}")),
                };
            }
            "--size" => a.size = Some(it.next().ok_or("--size needs a value")?.parse().map_err(|_| "bad --size")?),
            "--quality" => a.quality = Some(it.next().ok_or("--quality needs a value")?.parse().map_err(|_| "bad --quality")?),
            "--bitrate-kbps" => a.bitrate_kbps = it.next().ok_or("--bitrate-kbps needs a value")?.parse().map_err(|_| "bad --bitrate-kbps")?,
            "--fps" => a.fps = Some(it.next().ok_or("--fps needs a value")?.parse().map_err(|_| "bad --fps")?),
            "--tile" => a.tile = Some(it.next().ok_or("--tile needs a value")?.parse().map_err(|_| "bad --tile")?),
            "--no-native" => a.native = false,
            "--host" => a.host = it.next().ok_or("--host needs a value")?,
            "--port" => a.port = it.next().ok_or("--port needs a value")?.parse().map_err(|_| "bad --port")?,
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    if a.size.is_some() {
        // Explicit size disables native unless caller asked for it.
        a.native = false;
    }
    Ok(a)
}

#[derive(Debug)]
enum UserEvent {
    NewFrame { bytes: u32 },
    Disconnected(String),
}

/// Latest frame ready to present. Two backings depending on the codec:
/// `Bgra` is a CPU-side BGRA8888 buffer (JPEG / tilejpeg paths); `Hardware`
/// is a CVPixelBuffer that lives entirely on the GPU (H.264 / VideoToolbox).
enum DecodedFrame {
    Bgra {
        rgb: Vec<u8>,
        width: u32,
        height: u32,
    },
    #[cfg(target_os = "macos")]
    Hardware {
        pb: videotoolbox_decoder::PixelBuffer,
    },
}

/// Window→device mapping snapshot, refreshed every render.
#[derive(Clone, Copy, Debug)]
struct Mapping {
    pad_x: u32,
    pad_y: u32,
    out_w: u32,
    out_h: u32,
    src_w: u32,
    src_h: u32,
}

impl Mapping {
    /// Convert window-space coordinates to device pixels.
    /// Returns None if the point falls outside the letterboxed device area.
    fn map(&self, wx: f64, wy: f64) -> Option<(i32, i32)> {
        let dx = wx as i32 - self.pad_x as i32;
        let dy = wy as i32 - self.pad_y as i32;
        if dx < 0 || dy < 0 || dx as u32 >= self.out_w || dy as u32 >= self.out_h {
            return None;
        }
        Some((
            (dx as i64 * self.src_w as i64 / self.out_w.max(1) as i64) as i32,
            (dy as i64 * self.src_h as i64 / self.out_h.max(1) as i64) as i32,
        ))
    }
}

#[derive(Debug)]
enum InputCmd {
    PointerDown(i32, i32),
    PointerMove(i32, i32),
    PointerUp(i32, i32),
    Scroll { x: i32, y: i32, dy: i32 },
    KeyName(&'static str),
    Text(String),
}

struct App {
    window: Option<Arc<Window>>,
    #[cfg(target_os = "macos")]
    renderer: Option<MetalRenderer>,
    latest: Arc<Mutex<Option<DecodedFrame>>>,
    frame_count: u64,
    bytes_count: u64,
    last_log: Instant,
    codec_label: &'static str,
    // Input forwarding state.
    mapping: Arc<Mutex<Option<Mapping>>>,
    cursor_pos: PhysicalPosition<f64>,
    dragging: bool,
    input_tx: Option<Sender<InputCmd>>,
    // Device's actual display dimensions in pixels (from `info`).
    device_size: Option<(u32, u32)>,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let title = format!("hs — device mirror ({})", self.codec_label);
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(winit::dpi::LogicalSize::new(INITIAL_W, INITIAL_H));
        let win = Arc::new(event_loop.create_window(attrs).expect("create window"));

        // Attach a CAMetalLayer directly to the NSView. Drawable size is the
        // device's native pixel dimensions; the layer scales aspect-fit to the
        // current view bounds (window size). Each frame we upload our BGRA
        // framebuffer straight into the drawable's texture and present —
        // skipping the ~16–32 ms WindowServer compositor batching that
        // softbuffer's CALayer path pays.
        let (dev_w, dev_h) = self.device_size.unwrap_or((1440, 3120));
        #[cfg(target_os = "macos")]
        {
            match MetalRenderer::new(&win, dev_w, dev_h) {
                Some(r) => {
                    eprintln!("viewer: Metal renderer attached ({}x{} device texture)", dev_w, dev_h);
                    self.renderer = Some(r);
                }
                None => {
                    eprintln!("viewer: failed to set up Metal renderer — window will be blank");
                }
            }
        }

        self.window = Some(win);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::Resized(new_size) => {
                #[cfg(target_os = "macos")]
                if let Some(r) = self.renderer.as_ref() {
                    r.resize_window(new_size.width.max(1), new_size.height.max(1));
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = position;
                if self.dragging {
                    if let Some((dx, dy)) = self.current_device_xy() {
                        self.send(InputCmd::PointerMove(dx, dy));
                    }
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if let Some((dx, dy)) = self.current_device_xy() {
                    match state {
                        ElementState::Pressed => {
                            self.dragging = true;
                            self.send(InputCmd::PointerDown(dx, dy));
                        }
                        ElementState::Released => {
                            if self.dragging {
                                self.send(InputCmd::PointerUp(dx, dy));
                                self.dragging = false;
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some((dx, dy)) = self.current_device_xy() {
                    let pixels = match delta {
                        MouseScrollDelta::LineDelta(_, lines) => (lines as f64 * 80.0).round() as i32,
                        MouseScrollDelta::PixelDelta(p) => p.y as i32,
                    };
                    if pixels != 0 {
                        self.send(InputCmd::Scroll { x: dx, y: dy, dy: pixels });
                    }
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                if ke.state != ElementState::Pressed {
                    // We only forward press events; the daemon's `key` command
                    // emits a press (DOWN+UP) and `text` is character-level.
                    return;
                }
                // Exit only via the window close button — Q is a normal typed char.
                let mapped = match &ke.logical_key {
                    Key::Named(NamedKey::Escape) => Some(InputCmd::KeyName("BACK")),
                    Key::Named(NamedKey::F1) => Some(InputCmd::KeyName("HOME")),
                    Key::Named(NamedKey::F2) => Some(InputCmd::KeyName("RECENTS")),
                    Key::Named(NamedKey::F3) => Some(InputCmd::KeyName("MENU")),
                    Key::Named(NamedKey::Enter) => Some(InputCmd::KeyName("ENTER")),
                    Key::Named(NamedKey::Backspace) => Some(InputCmd::KeyName("DEL")),
                    Key::Named(NamedKey::Tab) => Some(InputCmd::KeyName("TAB")),
                    Key::Named(NamedKey::ArrowUp) => Some(InputCmd::KeyName("DPAD_UP")),
                    Key::Named(NamedKey::ArrowDown) => Some(InputCmd::KeyName("DPAD_DOWN")),
                    Key::Named(NamedKey::ArrowLeft) => Some(InputCmd::KeyName("DPAD_LEFT")),
                    Key::Named(NamedKey::ArrowRight) => Some(InputCmd::KeyName("DPAD_RIGHT")),
                    Key::Named(NamedKey::Space) => Some(InputCmd::Text(" ".into())),
                    _ => None,
                };
                if let Some(cmd) = mapped {
                    self.send(cmd);
                } else if let Some(text) = ke.text.as_ref() {
                    // ASCII / typed character — daemon's `text` handles via KeyCharacterMap.
                    self.send(InputCmd::Text(text.to_string()));
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::NewFrame { bytes } => {
                self.frame_count += 1;
                self.bytes_count += bytes as u64;
                if self.last_log.elapsed().as_secs() >= 5 {
                    let secs = self.last_log.elapsed().as_secs_f64();
                    let fps = self.frame_count as f64 / secs;
                    let avg_kb =
                        (self.bytes_count as f64 / 1024.0) / self.frame_count.max(1) as f64;
                    let mbps = (self.bytes_count as f64 * 8.0) / (secs * 1_000_000.0);
                    eprintln!(
                        "viewer({}): {:5.1} fps  {:6.2} Mbps  avg-frame {:6.1} KB  over {:.1}s",
                        self.codec_label, fps, mbps, avg_kb, secs,
                    );
                    self.frame_count = 0;
                    self.bytes_count = 0;
                    self.last_log = Instant::now();
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            UserEvent::Disconnected(msg) => {
                eprintln!("disconnected: {msg}");
                event_loop.exit();
            }
        }
    }
}

impl App {
    fn render(&mut self) {
        #[cfg(target_os = "macos")]
        {
            let Some(r) = self.renderer.as_mut() else { return };
            let guard = self.latest.lock().unwrap();
            match guard.as_ref() {
                Some(DecodedFrame::Bgra { rgb, width, height }) => {
                    // BGRA8888 at frame dimensions (smaller than the device
                    // for H.264 paths capped to 2048; renderer reconfigures
                    // its drawable each call).
                    r.present(rgb, *width, *height);
                }
                Some(DecodedFrame::Hardware { pb }) => {
                    r.present_pixel_buffer(pb);
                }
                None => {}
            }
        }
    }

    /// Map the current cursor position to device pixel coordinates.
    /// On macOS this asks the Metal renderer which knows the drawable size +
    /// current window bounds.
    fn current_device_xy(&self) -> Option<(i32, i32)> {
        #[cfg(target_os = "macos")]
        {
            let renderer = self.renderer.as_ref()?;
            let window = self.window.as_ref()?;
            let size = window.inner_size();
            return renderer.window_pos_to_pixel(
                size.width.max(1),
                size.height.max(1),
                self.cursor_pos.x,
                self.cursor_pos.y,
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// Push an input command toward the input thread. Drops silently if the
    /// channel is closed (input thread died).
    fn send(&self, cmd: InputCmd) {
        if let Some(tx) = self.input_tx.as_ref() {
            let _ = tx.send(cmd);
        }
    }
}

// ---------- input thread ----------

fn input_thread(host: String, port: u16, rx: Receiver<InputCmd>) {
    // Persistent socket; on error, drop the channel by exiting.
    let sock = match TcpStream::connect((host.as_str(), port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("viewer: input socket connect failed: {e}");
            return;
        }
    };
    let _ = sock.set_nodelay(true);
    let _ = sock.set_read_timeout(Some(Duration::from_secs(5)));
    let mut sock = sock;
    let mut hdr = [0u8; 4];
    loop {
        let next = match rx.recv() {
            Ok(c) => c,
            Err(_) => return, // channel closed; viewer exiting
        };
        // Coalesce consecutive PointerMove events — only the most recent
        // matters; dropping intermediates keeps the wire / encoder unstrained.
        let cmd = if let InputCmd::PointerMove(_, _) = next {
            let mut latest = next;
            loop {
                match rx.try_recv() {
                    Ok(InputCmd::PointerMove(x, y)) => latest = InputCmd::PointerMove(x, y),
                    Ok(other) => {
                        // Send the pending move, then the new non-move event.
                        if send_one_with_ack(&mut sock, &latest, &mut hdr).is_err() {
                            return;
                        }
                        if send_one_with_ack(&mut sock, &other, &mut hdr).is_err() {
                            return;
                        }
                        latest = match rx.recv() { Ok(c) => c, Err(_) => return };
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            latest
        } else {
            next
        };
        if send_one_with_ack(&mut sock, &cmd, &mut hdr).is_err() {
            return;
        }
    }
}

fn send_one_with_ack(
    sock: &mut TcpStream,
    cmd: &InputCmd,
    hdr: &mut [u8; 4],
) -> std::io::Result<()> {
    let wire = match cmd {
        InputCmd::PointerDown(x, y) => format!("down x={x} y={y}"),
        InputCmd::PointerMove(x, y) => format!("move x={x} y={y}"),
        InputCmd::PointerUp(x, y) => format!("up x={x} y={y}"),
        InputCmd::Scroll { x, y, dy } => format!("scroll x={x} y={y} dy={dy}"),
        InputCmd::KeyName(name) => format!("key {name}"),
        InputCmd::Text(s) => format!("text {s}"),
    };
    let bytes = wire.as_bytes();
    sock.write_all(&(bytes.len() as u32).to_be_bytes())?;
    sock.write_all(bytes)?;
    // Read the short ack (ok / ERR:...). We don't act on errors beyond logging.
    sock.read_exact(hdr)?;
    let n = u32::from_be_bytes(*hdr) as usize;
    if n == 0 || n > 4096 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad ack len {n}"),
        ));
    }
    let mut buf = vec![0u8; n];
    sock.read_exact(&mut buf)?;
    if buf.starts_with(b"ERR:") {
        eprintln!(
            "viewer: input '{}' -> {}",
            wire,
            String::from_utf8_lossy(&buf)
        );
    }
    Ok(())
}

// ---------- network thread, JPEG ----------

fn network_thread_jpeg(
    args: Args,
    latest: Arc<Mutex<Option<DecodedFrame>>>,
    proxy: EventLoopProxy<UserEvent>,
) {
    let mut sock = match TcpStream::connect((args.host.as_str(), args.port)) {
        Ok(s) => s,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::Disconnected(format!("connect failed: {e}")));
            return;
        }
    };
    let _ = sock.set_nodelay(true);

    let mut cmd = String::from("stream");
    if args.native {
        cmd.push_str(" max=1");
    } else if let Some(s) = args.size {
        cmd.push_str(&format!(" size={s}"));
    }
    if let Some(q) = args.quality {
        cmd.push_str(&format!(" q={q}"));
    }
    if let Some(f) = args.fps {
        cmd.push_str(&format!(" fps={f}"));
    }
    if write_cmd(&mut sock, cmd.as_bytes()).is_err() {
        let _ = proxy.send_event(UserEvent::Disconnected("stream write failed".into()));
        return;
    }

    let mut len_buf = [0u8; 4];
    let mut jpeg_buf: Vec<u8> = Vec::with_capacity(256 * 1024);

    loop {
        if sock.read_exact(&mut len_buf).is_err() {
            let _ = proxy.send_event(UserEvent::Disconnected("read header eof".into()));
            return;
        }
        let n = u32::from_be_bytes(len_buf) as usize;
        if n == 0 || n > 256 * 1024 * 1024 {
            let _ = proxy.send_event(UserEvent::Disconnected(format!("bad length {n}")));
            return;
        }
        jpeg_buf.resize(n, 0);
        if sock.read_exact(&mut jpeg_buf).is_err() {
            let _ = proxy.send_event(UserEvent::Disconnected("read payload eof".into()));
            return;
        }

        if jpeg_buf.len() >= 4 && &jpeg_buf[..4] == b"ERR:" {
            let msg = String::from_utf8_lossy(&jpeg_buf).into_owned();
            let _ = proxy.send_event(UserEvent::Disconnected(msg));
            return;
        }

        let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
        let mut decoder = JpegDecoder::new_with_options(jpeg_buf.as_slice(), opts);
        match decoder.decode() {
            Ok(rgba) => {
                let info = match decoder.info() {
                    Some(i) => i,
                    None => continue,
                };
                let frame = DecodedFrame::Bgra {
                    rgb: rgba,
                    width: info.width as u32,
                    height: info.height as u32,
                };
                *latest.lock().unwrap() = Some(frame);
                let _ = proxy.send_event(UserEvent::NewFrame { bytes: n as u32 });
            }
            Err(e) => {
                eprintln!("jpeg decode error: {e:?}");
            }
        }
    }
}

// ---------- network thread, H.264 ----------

#[cfg(not(target_os = "macos"))]
fn network_thread_h264(
    _args: Args,
    _latest: Arc<Mutex<Option<DecodedFrame>>>,
    proxy: EventLoopProxy<UserEvent>,
) {
    let _ = proxy.send_event(UserEvent::Disconnected(
        "h264 decode is only implemented on macOS (VideoToolbox)".into(),
    ));
}

#[cfg(target_os = "macos")]
fn network_thread_h264(
    args: Args,
    latest: Arc<Mutex<Option<DecodedFrame>>>,
    proxy: EventLoopProxy<UserEvent>,
) {
    use videotoolbox_decoder::VtDecoder;

    let mut sock = match TcpStream::connect((args.host.as_str(), args.port)) {
        Ok(s) => s,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::Disconnected(format!("connect failed: {e}")));
            return;
        }
    };
    let _ = sock.set_nodelay(true);

    let mut cmd = String::from("stream_h264");
    if args.native {
        cmd.push_str(" max=1");
    } else if let Some(s) = args.size {
        cmd.push_str(&format!(" size={s}"));
    }
    cmd.push_str(&format!(" bitrate={}", args.bitrate_kbps));
    if let Some(f) = args.fps {
        cmd.push_str(&format!(" fps={f}"));
    }
    if write_cmd(&mut sock, cmd.as_bytes()).is_err() {
        let _ = proxy.send_event(UserEvent::Disconnected("stream_h264 write failed".into()));
        return;
    }

    let mut dec_logger = DecLogger::new("h264");
    let mut decoder = VtDecoder::new();

    let mut len_buf = [0u8; 4];
    let mut pkt: Vec<u8> = Vec::with_capacity(256 * 1024);

    loop {
        if sock.read_exact(&mut len_buf).is_err() {
            let _ = proxy.send_event(UserEvent::Disconnected("read header eof".into()));
            return;
        }
        let n = u32::from_be_bytes(len_buf) as usize;
        if n == 0 || n > 256 * 1024 * 1024 {
            let _ = proxy.send_event(UserEvent::Disconnected(format!("bad length {n}")));
            return;
        }
        pkt.resize(n, 0);
        if sock.read_exact(&mut pkt).is_err() {
            let _ = proxy.send_event(UserEvent::Disconnected("read payload eof".into()));
            return;
        }

        if pkt.len() >= 4 && &pkt[..4] == b"ERR:" {
            let msg = String::from_utf8_lossy(&pkt).into_owned();
            let _ = proxy.send_event(UserEvent::Disconnected(msg));
            return;
        }

        let t_dec_start = std::time::Instant::now();
        match decoder.decode(&pkt) {
            Ok(Some(pb)) => {
                let dec_ms = t_dec_start.elapsed().as_secs_f64() * 1000.0;
                *latest.lock().unwrap() = Some(DecodedFrame::Hardware { pb });
                dec_logger.tick(dec_ms);
                let _ = proxy.send_event(UserEvent::NewFrame { bytes: n as u32 });
            }
            Ok(None) => {
                // Config-only packet (SPS/PPS before first IDR) — no frame.
            }
            Err(e) => {
                eprintln!("h264 decode error: {e}");
            }
        }
    }
}

/// Rolling per-frame decode-time logger. Coalesces output to one line every
/// ~60 frames so we can see decode latency without spamming.
struct DecLogger {
    samples: Vec<f64>,
    label: &'static str,
}
impl DecLogger {
    fn new(label: &'static str) -> Self {
        Self { samples: Vec::with_capacity(64), label }
    }
    fn tick(&mut self, ms: f64) {
        self.samples.push(ms);
        if self.samples.len() < 60 { return; }
        self.samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let min = self.samples[0];
        let p50 = self.samples[self.samples.len() / 2];
        let p95 = self.samples[(self.samples.len() as f64 * 0.95) as usize];
        let max = self.samples[self.samples.len() - 1];
        eprintln!(
            "viewer({}-decode): n={}  min={:.1}ms  p50={:.1}ms  p95={:.1}ms  max={:.1}ms",
            self.label, self.samples.len(), min, p50, p95, max,
        );
        self.samples.clear();
    }
}

// ---------- network thread, tilejpeg ----------

const TILE_TYPE_DELTA: u8 = 0x00;
const TILE_TYPE_KEYFRAME: u8 = 0x01;

fn network_thread_tilejpeg(
    args: Args,
    latest: Arc<Mutex<Option<DecodedFrame>>>,
    proxy: EventLoopProxy<UserEvent>,
) {
    let mut sock = match TcpStream::connect((args.host.as_str(), args.port)) {
        Ok(s) => s,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::Disconnected(format!("connect failed: {e}")));
            return;
        }
    };
    let _ = sock.set_nodelay(true);

    // Device dimensions are required for tile geometry. Fetch via info.
    let (dev_w, dev_h) = match fetch_device_size(args.host.as_str(), args.port) {
        Ok(d) => d,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::Disconnected(format!("info failed: {e}")));
            return;
        }
    };
    let tile_edge: u32 = args.tile.unwrap_or(128);
    let tiles_x: u32 = (dev_w + tile_edge - 1) / tile_edge;
    let _tiles_y: u32 = (dev_h + tile_edge - 1) / tile_edge;

    // Build wire command.
    let mut cmd = String::from("stream_tilejpeg");
    if args.native {
        cmd.push_str(" max=1");
    } else if let Some(s) = args.size {
        cmd.push_str(&format!(" size={s}"));
    }
    if let Some(q) = args.quality {
        cmd.push_str(&format!(" q={q}"));
    }
    cmd.push_str(&format!(" tile={tile_edge}"));
    if write_cmd(&mut sock, cmd.as_bytes()).is_err() {
        let _ = proxy.send_event(UserEvent::Disconnected("stream_tilejpeg write failed".into()));
        return;
    }

    // Persistent RGBA framebuffer at device size. Patched in-place by deltas
    // and replaced wholesale by keyframes.
    let fb_len = (dev_w as usize) * (dev_h as usize) * 4;
    let mut fb: Vec<u8> = vec![0u8; fb_len];

    let mut len_buf = [0u8; 4];
    let mut pkt: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut dec_logger = DecLogger::new("tilejpeg");

    loop {
        if sock.read_exact(&mut len_buf).is_err() {
            let _ = proxy.send_event(UserEvent::Disconnected("read header eof".into()));
            return;
        }
        let n = u32::from_be_bytes(len_buf) as usize;
        if n == 0 || n > 256 * 1024 * 1024 {
            let _ = proxy.send_event(UserEvent::Disconnected(format!("bad length {n}")));
            return;
        }
        pkt.resize(n, 0);
        if sock.read_exact(&mut pkt).is_err() {
            let _ = proxy.send_event(UserEvent::Disconnected("read payload eof".into()));
            return;
        }
        if pkt.len() >= 4 && &pkt[..4] == b"ERR:" {
            let msg = String::from_utf8_lossy(&pkt).into_owned();
            let _ = proxy.send_event(UserEvent::Disconnected(msg));
            return;
        }

        let t_dec = std::time::Instant::now();
        let kind = pkt[0];
        match kind {
            TILE_TYPE_KEYFRAME => {
                if pkt.len() < 5 {
                    eprintln!("tilejpeg: short keyframe packet");
                    continue;
                }
                let jlen = u32::from_be_bytes([pkt[1], pkt[2], pkt[3], pkt[4]]) as usize;
                if pkt.len() < 5 + jlen {
                    eprintln!("tilejpeg: keyframe length mismatch");
                    continue;
                }
                let jpeg = &pkt[5..5 + jlen];
                match decode_jpeg_rgba(jpeg) {
                    Some((w, h, rgb)) => {
                        if (w as u32, h as u32) == (dev_w, dev_h) && rgb.len() == fb.len() {
                            fb.copy_from_slice(&rgb);
                        } else {
                            // Source dimensions don't match — letterbox into fb.
                            blit_rgba_into(&mut fb, dev_w as usize, 0, 0,
                                          w as usize, h as usize, &rgb);
                        }
                    }
                    None => continue,
                }
            }
            TILE_TYPE_DELTA => {
                if pkt.len() < 3 {
                    eprintln!("tilejpeg: short delta packet");
                    continue;
                }
                let num = u16::from_be_bytes([pkt[1], pkt[2]]) as usize;
                let mut cursor = 3;
                for _ in 0..num {
                    if cursor + 10 > pkt.len() { break; }
                    let idx = u16::from_be_bytes([pkt[cursor], pkt[cursor + 1]]) as u32;
                    let tw = u16::from_be_bytes([pkt[cursor + 2], pkt[cursor + 3]]) as u32;
                    let th = u16::from_be_bytes([pkt[cursor + 4], pkt[cursor + 5]]) as u32;
                    let jlen = u32::from_be_bytes([
                        pkt[cursor + 6], pkt[cursor + 7], pkt[cursor + 8], pkt[cursor + 9],
                    ]) as usize;
                    cursor += 10;
                    if cursor + jlen > pkt.len() { break; }
                    let jpeg = &pkt[cursor..cursor + jlen];
                    cursor += jlen;
                    if let Some((w, h, rgb)) = decode_jpeg_rgba(jpeg) {
                        if w as u32 != tw || h as u32 != th {
                            eprintln!(
                                "tilejpeg: tile size mismatch (jpeg {}x{}, hdr {}x{})",
                                w, h, tw, th
                            );
                            continue;
                        }
                        let row = idx / tiles_x;
                        let col = idx % tiles_x;
                        let dx = (col * tile_edge) as usize;
                        let dy = (row * tile_edge) as usize;
                        blit_rgba_into(
                            &mut fb, dev_w as usize, dx, dy,
                            tw as usize, th as usize, &rgb,
                        );
                    }
                }
            }
            _ => {
                eprintln!("tilejpeg: unknown packet type {kind:#x}");
                continue;
            }
        }

        dec_logger.tick(t_dec.elapsed().as_secs_f64() * 1000.0);

        // Publish the patched framebuffer to the UI thread.
        *latest.lock().unwrap() = Some(DecodedFrame::Bgra {
            rgb: fb.clone(),
            width: dev_w,
            height: dev_h,
        });
        let _ = proxy.send_event(UserEvent::NewFrame { bytes: n as u32 });
    }
}

/// Decode a JPEG byte slice to an RGBA8888 buffer using zune-jpeg.
fn decode_jpeg_rgba(jpeg: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut dec = JpegDecoder::new_with_options(jpeg, opts);
    let rgba = dec.decode().ok()?;
    let info = dec.info()?;
    Some((info.width as usize, info.height as usize, rgba))
}

/// Copy a tile from `src` (RGBA8888, tw*th*4 bytes) into `fb` at offset (dx, dy).
/// `fb_w` is the row stride in pixels of the destination framebuffer.
fn blit_rgba_into(
    fb: &mut [u8], fb_w: usize, dx: usize, dy: usize,
    tw: usize, th: usize, src: &[u8],
) {
    if tw == 0 || th == 0 { return; }
    let row_bytes = tw * 4;
    for y in 0..th {
        let src_off = y * row_bytes;
        let dst_off = ((dy + y) * fb_w + dx) * 4;
        if dst_off + row_bytes > fb.len() { break; }
        if src_off + row_bytes > src.len() { break; }
        fb[dst_off..dst_off + row_bytes]
            .copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
}

fn write_cmd(sock: &mut TcpStream, cmd: &[u8]) -> std::io::Result<()> {
    sock.write_all(&(cmd.len() as u32).to_be_bytes())?;
    sock.write_all(cmd)
}

/// One-shot `info` request — returns the device display dimensions in pixels.
fn fetch_device_size(host: &str, port: u16) -> std::io::Result<(u32, u32)> {
    let mut s = TcpStream::connect((host, port))?;
    let _ = s.set_nodelay(true);
    let cmd = b"info";
    s.write_all(&(cmd.len() as u32).to_be_bytes())?;
    s.write_all(cmd)?;
    let mut hdr = [0u8; 4];
    s.read_exact(&mut hdr)?;
    let n = u32::from_be_bytes(hdr) as usize;
    if n == 0 || n > 256 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad info length",
        ));
    }
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf)?;
    let text = std::str::from_utf8(&buf)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "info not utf-8"))?;
    let mut parts = text.split_whitespace();
    let w: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no width"))?;
    let h: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no height"))?;
    Ok((w, h))
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };
    eprintln!(
        "viewer: codec={:?} bitrate={} kbps native={} host={}:{}",
        args.codec, args.bitrate_kbps, args.native, args.host, args.port
    );

    // Query device display size — required so input coordinates land in the
    // right place even when the mirror is downscaled.
    let device_size = match fetch_device_size(&args.host, args.port) {
        Ok((w, h)) => {
            eprintln!("viewer: device display = {w}x{h}");
            Some((w, h))
        }
        Err(e) => {
            eprintln!(
                "viewer: WARN failed to query device size ({e}); clicks may land in the wrong place"
            );
            None
        }
    };

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("event loop");
    let proxy = event_loop.create_proxy();
    let latest: Arc<Mutex<Option<DecodedFrame>>> = Arc::new(Mutex::new(None));

    let codec_label: &'static str = match args.codec {
        Codec::Jpeg => "jpeg",
        Codec::H264 => "h264",
        Codec::TileJpeg => "tilejpeg",
    };

    {
        let latest = latest.clone();
        let proxy = proxy.clone();
        let args = args.clone();
        let spawn_fn = match args.codec {
            Codec::Jpeg => network_thread_jpeg,
            Codec::H264 => network_thread_h264,
            Codec::TileJpeg => network_thread_tilejpeg,
        };
        std::thread::Builder::new()
            .name("hs-net".into())
            .spawn(move || spawn_fn(args, latest, proxy))
            .expect("spawn net thread");
    }

    // Input channel: UI thread → input thread → daemon.
    let (input_tx, input_rx): (Sender<InputCmd>, Receiver<InputCmd>) = mpsc::channel();
    {
        let host = args.host.clone();
        let port = args.port;
        std::thread::Builder::new()
            .name("hs-input".into())
            .spawn(move || input_thread(host, port, input_rx))
            .expect("spawn input thread");
    }

    let mapping: Arc<Mutex<Option<Mapping>>> = Arc::new(Mutex::new(None));

    let mut app = App {
        window: None,
        #[cfg(target_os = "macos")]
        renderer: None,
        latest,
        frame_count: 0,
        bytes_count: 0,
        last_log: Instant::now(),
        codec_label,
        mapping,
        cursor_pos: PhysicalPosition::new(0.0, 0.0),
        dragging: false,
        input_tx: Some(input_tx),
        device_size,
    };
    event_loop.run_app(&mut app).expect("run_app");
}
