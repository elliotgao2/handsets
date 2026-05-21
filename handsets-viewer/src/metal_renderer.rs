// macOS-only Metal-backed renderer.
//
// Skips `softbuffer`'s `CALayer` + Core Animation compositor path, which on
// macOS adds ~16–32 ms of WindowServer batching latency per frame. We attach
// a `CAMetalLayer` directly to the window's NSView and use Metal to copy
// our RGBA framebuffer into the drawable's texture, then present.
//
// Drawable size is fixed at the device's native pixel dimensions; the layer's
// view bounds (set by NSView resize) handle visual letterboxing automatically
// via the contentsGravity = resizeAspect default.

#![cfg(target_os = "macos")]

use core_graphics_types::geometry::CGSize;
use metal::foreign_types::ForeignType;
use metal::{
    CommandQueue, Device, MTLOrigin, MTLPixelFormat, MTLRegion, MTLSize, MetalLayer, Texture,
    TextureDescriptor,
};
use objc::runtime::{Object, BOOL, YES};
use objc::{msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::videotoolbox_decoder::PixelBuffer;

pub struct MetalRenderer {
    #[allow(dead_code)] // kept alive for layer lifetime
    device: Device,
    queue: CommandQueue,
    layer: MetalLayer,
    /// Current drawable size — auto-reconfigured on every `present()` call
    /// whose framebuffer dimensions differ.
    fb_w: u32,
    fb_h: u32,
    /// Device's actual pixel dimensions, fixed at construction. Used for
    /// converting cursor→device coordinates for input injection (those want
    /// device pixels, not frame-buffer pixels).
    dev_w: u32,
    dev_h: u32,
}

impl MetalRenderer {
    pub fn new(window: &Window, dev_w: u32, dev_h: u32) -> Option<Self> {
        let fb_w = dev_w;
        let fb_h = dev_h;
        // Resolve the underlying NSView from winit (raw-window-handle 0.6).
        let handle = window.window_handle().ok()?.as_raw();
        let ns_view_ptr: *mut Object = match handle {
            RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as *mut Object,
            _ => return None,
        };
        if ns_view_ptr.is_null() {
            return None;
        }

        let device = Device::system_default()?;
        let queue = device.new_command_queue();
        let layer = MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        layer.set_framebuffer_only(true);
        layer.set_presents_with_transaction(false);
        layer.set_drawable_size(CGSize::new(fb_w as f64, fb_h as f64));

        // Attach the Metal layer to the NSView. We replace the view's layer
        // entirely; `setWantsLayer:YES` is required to opt the view into
        // layer-backed rendering before macOS will look at `layer`.
        unsafe {
            let _: () = msg_send![ns_view_ptr, setWantsLayer: YES];
            let _: () = msg_send![ns_view_ptr, setLayer: layer.as_ptr()];
        }

        Some(Self {
            device,
            queue,
            layer,
            fb_w,
            fb_h,
            dev_w,
            dev_h,
        })
    }

    /// Drawable size is fixed at the framebuffer (device) resolution; we don't
    /// touch it on window resize. The visible scaling happens because the
    /// layer's view bounds change with the window, and `contentsGravity` keeps
    /// the aspect ratio. This avoids reallocating the drawable per resize.
    pub fn resize_window(&self, _w: u32, _h: u32) {
        // Intentionally empty — see comment above.
    }

    /// Upload the BGRA framebuffer to the next drawable and present.
    /// Reconfigures the layer's drawable size to (w, h) if it differs from
    /// the previous frame — supports codecs (e.g. H.264) that produce frames
    /// smaller than the device's native resolution due to encoder caps.
    pub fn present(&mut self, bgra: &[u8], w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        if w != self.fb_w || h != self.fb_h {
            self.layer
                .set_drawable_size(CGSize::new(w as f64, h as f64));
            self.fb_w = w;
            self.fb_h = h;
        }
        let expected = (w as usize) * (h as usize) * 4;
        if bgra.len() != expected {
            return;
        }
        let Some(drawable) = self.layer.next_drawable() else {
            return;
        };
        let texture = drawable.texture();
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: w as u64,
                height: h as u64,
                depth: 1,
            },
        };
        let bytes_per_row = (w as u64) * 4;
        texture.replace_region(region, 0, bgra.as_ptr() as *const _, bytes_per_row);

        let cmd_buf = self.queue.new_command_buffer();
        cmd_buf.present_drawable(&drawable);
        cmd_buf.commit();
    }

    /// Zero-copy present from a VideoToolbox-decoded `CVPixelBuffer`.
    ///
    /// The pixel buffer is IOSurface-backed (we set `kMetalCompatibility=true`
    /// on the decoder's destination attributes). We wrap the IOSurface as an
    /// `MTLTexture` and blit it straight into the drawable — CPU never touches
    /// the BGRA data, the decoder's media-engine writes go to the IOSurface
    /// and the GPU reads from the same IOSurface.
    pub fn present_pixel_buffer(&mut self, pb: &PixelBuffer) {
        let w = pb.width();
        let h = pb.height();
        if w == 0 || h == 0 {
            return;
        }
        let iosurface = pb.iosurface();
        if iosurface.is_null() {
            return;
        }

        if w != self.fb_w || h != self.fb_h {
            self.layer
                .set_drawable_size(CGSize::new(w as f64, h as f64));
            self.fb_w = w;
            self.fb_h = h;
        }

        // Build a descriptor matching the IOSurface and ask Metal to wrap it.
        // `newTextureWithDescriptor:iosurface:plane:` isn't exposed by the
        // `metal` crate so we call directly via the ObjC runtime. The
        // returned MTLTexture has +1 retain (ObjC `new...` convention) which
        // `Texture::from_ptr` takes ownership of.
        let descriptor = TextureDescriptor::new();
        descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        descriptor.set_width(w as u64);
        descriptor.set_height(h as u64);

        let device_obj = self.device.as_ptr() as *mut Object;
        let desc_obj = descriptor.as_ptr() as *mut Object;
        let tex_ptr: *mut Object = unsafe {
            msg_send![device_obj,
                newTextureWithDescriptor: desc_obj
                iosurface: iosurface
                plane: 0usize]
        };
        if tex_ptr.is_null() {
            return;
        }
        let src_texture: Texture = unsafe { Texture::from_ptr(tex_ptr as *mut _) };

        let Some(drawable) = self.layer.next_drawable() else {
            return;
        };

        let cmd_buf = self.queue.new_command_buffer();
        let blit = cmd_buf.new_blit_command_encoder();
        blit.copy_from_texture(
            &src_texture,
            0,
            0,
            MTLOrigin { x: 0, y: 0, z: 0 },
            MTLSize {
                width: w as u64,
                height: h as u64,
                depth: 1,
            },
            drawable.texture(),
            0,
            0,
            MTLOrigin { x: 0, y: 0, z: 0 },
        );
        blit.end_encoding();
        cmd_buf.present_drawable(&drawable);
        cmd_buf.commit();
        // `src_texture` drops here; the command buffer retains it (and
        // transitively the IOSurface) until GPU execution completes.
    }

    /// Map a cursor position (in window physical pixels) to **device** pixel
    /// coordinates, accounting for letterboxing. Returns None if outside the
    /// device-aspect rectangle within the window.
    ///
    /// Letterbox geometry uses the current `fb_w`/`fb_h` (what the layer is
    /// actually displaying), but the OUTPUT is mapped onto `dev_w`/`dev_h`
    /// because `UiAutomation.injectInputEvent` takes device pixels — not
    /// downscaled-mirror pixels.
    pub fn window_pos_to_pixel(
        &self,
        win_w: u32,
        win_h: u32,
        wx: f64,
        wy: f64,
    ) -> Option<(i32, i32)> {
        if win_w == 0 || win_h == 0 || self.fb_w == 0 || self.fb_h == 0 {
            return None;
        }
        let dw = win_w as f64;
        let dh = win_h as f64;
        let sw = self.fb_w as f64;
        let sh = self.fb_h as f64;
        let src_aspect = sw / sh;
        let dst_aspect = dw / dh;
        let (out_w, out_h) = if src_aspect > dst_aspect {
            (dw, dw / src_aspect)
        } else {
            (dh * src_aspect, dh)
        };
        let pad_x = (dw - out_w) * 0.5;
        let pad_y = (dh - out_h) * 0.5;
        let lx = wx - pad_x;
        let ly = wy - pad_y;
        if lx < 0.0 || ly < 0.0 || lx >= out_w || ly >= out_h {
            return None;
        }
        let device_x = lx * (self.dev_w as f64) / out_w;
        let device_y = ly * (self.dev_h as f64) / out_h;
        Some((device_x as i32, device_y as i32))
    }
}

// Suppress unused import warning for BOOL when compiling against newer objc.
#[allow(dead_code)]
const _: BOOL = YES;
