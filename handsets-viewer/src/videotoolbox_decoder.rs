// macOS hardware H.264 decoder using VideoToolbox.
//
// Hand-rolled FFI to CoreFoundation / CoreMedia / CoreVideo / VideoToolbox so
// we don't pull in extra crates. The decoder consumes the same Annex-B packets
// the Android H264Streamer emits (length-prefixed NAL access units) and emits
// IOSurface-backed CVPixelBuffers that the Metal renderer wraps as MTLTextures
// — CPU never touches BGRA pixel data once it leaves the network buffer.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::ptr;

// ---------- type aliases for the FFI surface ----------

type OSStatus = i32;
type Boolean = u8;
type CFAllocatorRef = *const c_void;
type CFTypeRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CMFormatDescriptionRef = *const c_void;
type CMSampleBufferRef = *const c_void;
type CMBlockBufferRef = *const c_void;
type VTDecompressionSessionRef = *const c_void;
type CVImageBufferRef = *const c_void;
pub type CVPixelBufferRef = *const c_void;
pub type IOSurfaceRef = *const c_void;

type CFIndex = isize;
type CFNumberType = isize;
type CMItemCount = isize;
type CMBlockBufferFlags = u32;
type VTDecodeFrameFlags = u32;
type VTDecodeInfoFlags = u32;

const K_CF_NUMBER_SINT32_TYPE: CFNumberType = 3;
const K_CV_PIXEL_FORMAT_TYPE_32_BGRA: u32 = 0x4247_5241; // 'BGRA'

#[repr(C)]
#[derive(Copy, Clone)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

type VTDecompressionOutputCallback = extern "C" fn(
    decompression_output_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTDecodeInfoFlags,
    image_buffer: CVImageBufferRef,
    presentation_time_stamp: CMTime,
    presentation_duration: CMTime,
);

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    callback: Option<VTDecompressionOutputCallback>,
    refcon: *mut c_void,
}

#[link(name = "VideoToolbox", kind = "framework")]
#[link(name = "CoreMedia", kind = "framework")]
#[link(name = "CoreVideo", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);

    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: CFNumberType,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;

    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    static kCFAllocatorNull: CFAllocatorRef;
    static kCFBooleanTrue: CFTypeRef;

    // CoreVideo constant CFStringRefs.
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferMetalCompatibilityKey: CFStringRef;

    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_description_out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;

    fn CMBlockBufferCreateWithMemoryBlock(
        structure_allocator: CFAllocatorRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: CMBlockBufferFlags,
        block_buffer_out: *mut CMBlockBufferRef,
    ) -> OSStatus;

    fn CMSampleBufferCreate(
        allocator: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        data_ready: Boolean,
        make_data_ready_callback: *const c_void,
        make_data_ready_refcon: *mut c_void,
        format_description: CMFormatDescriptionRef,
        num_samples: CMItemCount,
        num_sample_timing_entries: CMItemCount,
        sample_timing_array: *const c_void,
        num_sample_size_entries: CMItemCount,
        sample_size_array: *const usize,
        sample_buffer_out: *mut CMSampleBufferRef,
    ) -> OSStatus;

    fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        video_format_description: CMFormatDescriptionRef,
        video_decoder_specification: CFDictionaryRef,
        destination_image_buffer_attributes: CFDictionaryRef,
        output_callback: *const VTDecompressionOutputCallbackRecord,
        decompression_session_out: *mut VTDecompressionSessionRef,
    ) -> OSStatus;

    fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample_buffer: CMSampleBufferRef,
        decode_flags: VTDecodeFrameFlags,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut VTDecodeInfoFlags,
    ) -> OSStatus;

    fn VTDecompressionSessionWaitForAsynchronousFrames(
        session: VTDecompressionSessionRef,
    ) -> OSStatus;

    fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);

    fn CVPixelBufferGetIOSurface(pixel_buffer: CVPixelBufferRef) -> IOSurfaceRef;
    fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
}

// ---------- retained CVPixelBuffer wrapper ----------

/// Owns a +1 reference to a `CVPixelBuffer`. Drop releases.
///
/// VideoToolbox hands us pixel buffers as unretained references inside the
/// output callback; we `CFRetain` before storing one of these.
pub struct PixelBuffer {
    raw: CVPixelBufferRef,
}

impl PixelBuffer {
    fn retain_from_callback(raw: CVPixelBufferRef) -> Option<Self> {
        if raw.is_null() {
            return None;
        }
        unsafe { CFRetain(raw) };
        Some(Self { raw })
    }

    /// IOSurface backing the pixel buffer. Returns null if the buffer is not
    /// IOSurface-backed (shouldn't happen — we set kMetalCompatibility=true).
    pub fn iosurface(&self) -> IOSurfaceRef {
        unsafe { CVPixelBufferGetIOSurface(self.raw) }
    }

    pub fn width(&self) -> u32 {
        unsafe { CVPixelBufferGetWidth(self.raw) as u32 }
    }

    pub fn height(&self) -> u32 {
        unsafe { CVPixelBufferGetHeight(self.raw) as u32 }
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        unsafe { CFRelease(self.raw) };
    }
}

// CVPixelBuffer ref-counts are atomic; the buffer itself is safe to move
// between threads as long as Drop / CFRelease are paired with the prior
// CFRetain (they are).
unsafe impl Send for PixelBuffer {}
unsafe impl Sync for PixelBuffer {}

// ---------- the decoder ----------

pub struct VtDecoder {
    session: VTDecompressionSessionRef,
    format_desc: CMFormatDescriptionRef,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    /// Re-used scratch buffer for converting Annex-B start codes to AVCC
    /// 4-byte length prefixes per access unit.
    avcc_buf: Vec<u8>,
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
            }
            if !self.format_desc.is_null() {
                CFRelease(self.format_desc);
            }
        }
    }
}

struct DecodedSlot {
    pb: Option<PixelBuffer>,
    status: OSStatus,
}

extern "C" fn vt_output_callback(
    _decompression_output_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: CVImageBufferRef,
    _presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    if source_frame_ref_con.is_null() {
        return;
    }
    // SAFETY: `decode_avcc` passes a stack pointer to a DecodedSlot that lives
    // across `VTDecompressionSessionDecodeFrame` + WaitForAsynchronousFrames.
    let slot = unsafe { &mut *(source_frame_ref_con as *mut DecodedSlot) };
    slot.status = status;
    if status == 0 {
        slot.pb = PixelBuffer::retain_from_callback(image_buffer);
    }
}

impl VtDecoder {
    pub fn new() -> Self {
        Self {
            session: ptr::null(),
            format_desc: ptr::null(),
            sps: None,
            pps: None,
            avcc_buf: Vec::with_capacity(256 * 1024),
        }
    }

    /// Decode a length-prefixed Annex-B packet from the wire. Returns Some(pb)
    /// when a frame is produced, None for codec-config-only packets (SPS/PPS
    /// before the first IDR).
    pub fn decode(&mut self, packet: &[u8]) -> Result<Option<PixelBuffer>, String> {
        self.avcc_buf.clear();
        let mut new_params = false;
        for nal in AnnexBIter::new(packet) {
            if nal.is_empty() {
                continue;
            }
            let nal_type = nal[0] & 0x1f;
            match nal_type {
                7 => {
                    // SPS — store; only flag a rebuild if it changed.
                    if self.sps.as_deref() != Some(nal) {
                        self.sps = Some(nal.to_vec());
                        new_params = true;
                    }
                }
                8 => {
                    if self.pps.as_deref() != Some(nal) {
                        self.pps = Some(nal.to_vec());
                        new_params = true;
                    }
                }
                9 => {
                    // Access unit delimiter — drop, not part of the AVCC frame
                    // data the decoder wants.
                }
                _ => {
                    // Slice / SEI / others → AVCC with 4-byte big-endian length.
                    let len = nal.len() as u32;
                    self.avcc_buf.extend_from_slice(&len.to_be_bytes());
                    self.avcc_buf.extend_from_slice(nal);
                }
            }
        }

        if new_params && self.sps.is_some() && self.pps.is_some() {
            self.rebuild_session()?;
        }
        if self.session.is_null() || self.avcc_buf.is_empty() {
            return Ok(None);
        }
        self.decode_avcc()
    }

    fn rebuild_session(&mut self) -> Result<(), String> {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
                self.session = ptr::null();
            }
            if !self.format_desc.is_null() {
                CFRelease(self.format_desc);
                self.format_desc = ptr::null();
            }

            let sps = self.sps.as_ref().unwrap();
            let pps = self.pps.as_ref().unwrap();
            let pointers: [*const u8; 2] = [sps.as_ptr(), pps.as_ptr()];
            let sizes: [usize; 2] = [sps.len(), pps.len()];
            let mut fmt: CMFormatDescriptionRef = ptr::null();
            let s = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                ptr::null(),
                2,
                pointers.as_ptr(),
                sizes.as_ptr(),
                4,
                &mut fmt,
            );
            if s != 0 || fmt.is_null() {
                return Err(format!(
                    "CMVideoFormatDescriptionCreateFromH264ParameterSets: {s}"
                ));
            }
            self.format_desc = fmt;

            // Destination attrs:
            //   kCVPixelBufferPixelFormatTypeKey      = 32BGRA
            //   kCVPixelBufferMetalCompatibilityKey  = true  (forces IOSurface
            //                                                 backing usable
            //                                                 as MTLTexture)
            let pix_val = K_CV_PIXEL_FORMAT_TYPE_32_BGRA;
            let cf_num = CFNumberCreate(
                ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                &pix_val as *const _ as *const c_void,
            );
            if cf_num.is_null() {
                return Err("CFNumberCreate".into());
            }
            let keys: [*const c_void; 2] = [
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferMetalCompatibilityKey,
            ];
            let values: [*const c_void; 2] = [cf_num, kCFBooleanTrue];
            let dest = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                2,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            CFRelease(cf_num);
            if dest.is_null() {
                return Err("CFDictionaryCreate".into());
            }

            let cb = VTDecompressionOutputCallbackRecord {
                callback: Some(vt_output_callback),
                refcon: ptr::null_mut(),
            };

            let mut session: VTDecompressionSessionRef = ptr::null();
            let s = VTDecompressionSessionCreate(
                ptr::null(),
                fmt,
                ptr::null(),
                dest,
                &cb,
                &mut session,
            );
            CFRelease(dest);
            if s != 0 || session.is_null() {
                return Err(format!("VTDecompressionSessionCreate: {s}"));
            }
            self.session = session;
        }
        Ok(())
    }

    fn decode_avcc(&mut self) -> Result<Option<PixelBuffer>, String> {
        unsafe {
            // Wrap self.avcc_buf in a CMBlockBuffer with kCFAllocatorNull as
            // the block allocator — VT must NOT try to free Rust-owned memory.
            let mut block: CMBlockBufferRef = ptr::null();
            let s = CMBlockBufferCreateWithMemoryBlock(
                ptr::null(),
                self.avcc_buf.as_mut_ptr() as *mut c_void,
                self.avcc_buf.len(),
                kCFAllocatorNull,
                ptr::null(),
                0,
                self.avcc_buf.len(),
                0,
                &mut block,
            );
            if s != 0 || block.is_null() {
                return Err(format!("CMBlockBufferCreateWithMemoryBlock: {s}"));
            }

            let sizes: [usize; 1] = [self.avcc_buf.len()];
            let mut sample: CMSampleBufferRef = ptr::null();
            let s = CMSampleBufferCreate(
                ptr::null(),
                block,
                1, // dataReady = true
                ptr::null(),
                ptr::null_mut(),
                self.format_desc,
                1, // numSamples
                0, // no timing info — we present immediately
                ptr::null(),
                1,
                sizes.as_ptr(),
                &mut sample,
            );
            // Sample buffer retains the block buffer — safe to release our ref.
            CFRelease(block);
            if s != 0 || sample.is_null() {
                return Err(format!("CMSampleBufferCreate: {s}"));
            }

            let mut slot = DecodedSlot { pb: None, status: 0 };
            let mut info_flags: VTDecodeInfoFlags = 0;
            let s = VTDecompressionSessionDecodeFrame(
                self.session,
                sample,
                0, // no special flags — default sync-ish behaviour
                &mut slot as *mut _ as *mut c_void,
                &mut info_flags,
            );
            CFRelease(sample);
            if s != 0 {
                return Err(format!("VTDecompressionSessionDecodeFrame: {s}"));
            }
            // Flush any deferred decodes so the callback has run before we
            // return — avoids reusing self.avcc_buf while VT still references it.
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);

            if slot.status != 0 {
                return Err(format!("VT decode callback status: {}", slot.status));
            }
            Ok(slot.pb)
        }
    }
}

// ---------- Annex-B NAL splitter ----------

struct AnnexBIter<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> AnnexBIter<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

impl<'a> Iterator for AnnexBIter<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        let (_, nal_begin) = find_start_code(self.buf, self.pos)?;
        let nal_end = match find_start_code(self.buf, nal_begin) {
            Some((next_start, _)) => next_start,
            None => self.buf.len(),
        };
        self.pos = nal_end;
        if nal_begin >= nal_end {
            return None;
        }
        Some(&self.buf[nal_begin..nal_end])
    }
}

/// Returns (start_index, byte_after_start_code) for the next 3- or 4-byte
/// Annex-B start code at or after `from`.
fn find_start_code(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            if i + 4 <= buf.len() && buf[i + 2] == 0 && buf[i + 3] == 1 {
                return Some((i, i + 4));
            }
            if buf[i + 2] == 1 {
                return Some((i, i + 3));
            }
        }
        i += 1;
    }
    None
}
