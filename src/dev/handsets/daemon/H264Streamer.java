package dev.handsets.daemon;

import android.graphics.Point;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.view.Surface;

import android.os.Bundle;

import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.Collections;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;

/**
 * One H.264 streaming session over one client socket. Sets up a MediaCodec
 * AVC encoder whose input Surface is the producer for a VirtualDisplay
 * mirroring the default display. Output thread dequeues encoded access units
 * (Annex-B NAL units) and writes each as a length-prefixed wire packet.
 *
 * One encoder + one VirtualDisplay per connection. Released on disconnect.
 */
final class H264Streamer {

    private static final String MIME_AVC = "video/avc";
    // MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface — hard-coded to
    // avoid referring to the constant by name (some build paths complain).
    private static final int COLOR_FORMAT_SURFACE = 0x7F000789;
    private static final int VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR = 16;
    private static final long DEQUEUE_TIMEOUT_US = 100_000L;     // 100 ms
    /** Conservative encoder dimension cap. Android's stock software AVC
     *  encoder maxes out at 2048×2048 — querying CodecCapabilities at startup
     *  would be more precise, but this constant works on every device. */
    private static final int ENCODER_MAX_DIM = 2048;

    /** Registry of currently-running streamers so an out-of-band `keyframe`
     *  command from the request/response port can poke them all. */
    private static final Set<H264Streamer> ACTIVE =
            Collections.newSetFromMap(new ConcurrentHashMap<>());

    private final DisplayManager dm;
    private final Point sourceSize;
    private final int targetLongEdge;
    private final int bitrateKbps;
    private final int fps;
    private final int gopSec;
    private final DataOutputStream out;
    private volatile MediaCodec encoderRef;

    H264Streamer(DisplayManager dm,
                 Point sourceSize,
                 int targetLongEdge,
                 int bitrateKbps,
                 int fps,
                 int gopSec,
                 DataOutputStream out) {
        this.dm = dm;
        this.sourceSize = sourceSize;
        this.targetLongEdge = targetLongEdge;
        this.bitrateKbps = bitrateKbps > 0 ? bitrateKbps : 6000;
        this.fps = fps > 0 ? fps : 60;
        this.gopSec = gopSec > 0 ? gopSec : 1;
        this.out = out;
    }

    /** Signal that all currently-active h264 encoders should emit a key
     *  frame on their next output. Safe to call from any thread. */
    static void requestKeyframeAll() {
        for (H264Streamer s : ACTIVE) {
            s.requestKeyframe();
        }
    }

    static int activeCount() {
        return ACTIVE.size();
    }

    /** Ask this stream's encoder to mark the next emitted frame as IDR. */
    void requestKeyframe() {
        MediaCodec enc = this.encoderRef;
        if (enc == null) return;
        try {
            Bundle b = new Bundle();
            b.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0);
            enc.setParameters(b);
        } catch (Throwable ignored) {
            // Best-effort. setParameters can throw if the encoder is in a
            // transient state; we just skip and try again on the next call.
        }
    }

    void serve() {
        int srcW = sourceSize.x, srcH = sourceSize.y;
        if (srcW <= 0 || srcH <= 0) {
            writeErr("no-source-size");
            return;
        }
        int longEdge = Math.max(srcW, srcH);
        // Honour the caller's requested long-edge but also enforce the
        // encoder's hard dimension cap on BOTH axes (square 2048 ceiling).
        float scale = (float) targetLongEdge / longEdge;
        if (scale > 1f) scale = 1f;
        float capScale = Math.min(
                (float) ENCODER_MAX_DIM / srcW,
                (float) ENCODER_MAX_DIM / srcH);
        if (scale > capScale) scale = capScale;
        // H.264 requires even dimensions; round to nearest multiple of 2.
        int outW = roundEven(Math.max(2, Math.round(srcW * scale)));
        int outH = roundEven(Math.max(2, Math.round(srcH * scale)));
        if (outW > ENCODER_MAX_DIM) outW = ENCODER_MAX_DIM;
        if (outH > ENCODER_MAX_DIM) outH = ENCODER_MAX_DIM;

        MediaCodec encoder = null;
        Surface inputSurface = null;
        VirtualDisplay vd = null;
        try {
            MediaFormat fmt = MediaFormat.createVideoFormat(MIME_AVC, outW, outH);
            fmt.setInteger(MediaFormat.KEY_COLOR_FORMAT, COLOR_FORMAT_SURFACE);
            fmt.setInteger(MediaFormat.KEY_BIT_RATE, bitrateKbps * 1000);
            fmt.setInteger(MediaFormat.KEY_FRAME_RATE, fps);
            fmt.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, gopSec);
            // NOTE: tried KEY_PREPEND_HEADER_TO_SYNC_FRAMES and the vendor
            // "prepend-sps-pps-to-idr-frames" — the emulator's Codec2 AVC
            // encoder rejects configure() with these set. Each fresh
            // connection already emits BUFFER_FLAG_CODEC_CONFIG before its
            // first IDR, so we don't need them.
            // VBR — emulator's Codec2 encoder gets wedged on CBR + low-latency
            // vendor extension flags; sticking with the defaults that work.
            try {
                fmt.setInteger(MediaFormat.KEY_BITRATE_MODE,
                        MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR);
            } catch (Throwable ignored) {}
            // No B-frames → decode order == display order, no reorder buffer.
            try {
                fmt.setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0);
            } catch (Throwable ignored) {}

            encoder = MediaCodec.createEncoderByType(MIME_AVC);
            encoder.configure(fmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            inputSurface = encoder.createInputSurface();
            encoder.start();
            this.encoderRef = encoder;
            ACTIVE.add(this);

            vd = dm.createVirtualDisplay(
                    "hs-h264-" + outW + "x" + outH,
                    outW, outH, 320,
                    inputSurface,
                    VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR);
            if (vd == null) {
                writeErr("createVirtualDisplay-null");
                return;
            }

            System.out.println("h264: started " + outW + "x" + outH
                    + " @ " + bitrateKbps + " kbps  fps=" + fps);

            // Output loop.
            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            while (true) {
                int idx = encoder.dequeueOutputBuffer(info, DEQUEUE_TIMEOUT_US);
                if (idx == MediaCodec.INFO_TRY_AGAIN_LATER) {
                    continue;
                }
                if (idx == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    // Some encoders emit SPS/PPS here instead of via BUFFER_FLAG_CODEC_CONFIG.
                    // Pull them out of the new MediaFormat and send eagerly.
                    MediaFormat newFmt = encoder.getOutputFormat();
                    ByteBuffer csd0 = newFmt.getByteBuffer("csd-0");
                    ByteBuffer csd1 = newFmt.getByteBuffer("csd-1");
                    int total = (csd0 != null ? csd0.remaining() : 0)
                            + (csd1 != null ? csd1.remaining() : 0);
                    if (total > 0) {
                        byte[] csd = new byte[total];
                        int p = 0;
                        if (csd0 != null) {
                            int n = csd0.remaining();
                            csd0.get(csd, p, n);
                            p += n;
                        }
                        if (csd1 != null) csd1.get(csd, p, csd1.remaining());
                        writeFrame(csd);
                    }
                    continue;
                }
                if (idx < 0) continue; // INFO_OUTPUT_BUFFERS_CHANGED etc.

                ByteBuffer buf = encoder.getOutputBuffer(idx);
                if (buf != null && info.size > 0) {
                    buf.position(info.offset);
                    buf.limit(info.offset + info.size);
                    byte[] payload = new byte[info.size];
                    buf.get(payload);
                    try {
                        writeFrame(payload);
                    } catch (IOException disconnected) {
                        // Client gone — clean shutdown.
                        encoder.releaseOutputBuffer(idx, false);
                        return;
                    }
                }
                encoder.releaseOutputBuffer(idx, false);

                if ((info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0) {
                    return;
                }
            }
        } catch (Throwable t) {
            try { writeErr("h264-failed:" + t.getClass().getSimpleName()
                    + ":" + t.getMessage()); } catch (Throwable ignored) {}
            t.printStackTrace();
        } finally {
            ACTIVE.remove(this);
            this.encoderRef = null;
            if (vd != null) {
                try { vd.release(); } catch (Throwable ignored) {}
            }
            if (encoder != null) {
                try { encoder.stop(); } catch (Throwable ignored) {}
                try { encoder.release(); } catch (Throwable ignored) {}
            }
            if (inputSurface != null) {
                try { inputSurface.release(); } catch (Throwable ignored) {}
            }
        }
    }

    private void writeFrame(byte[] payload) throws IOException {
        out.writeInt(payload.length);
        out.write(payload);
        out.flush();
    }

    private void writeErr(String tail) {
        try {
            byte[] msg = ("ERR:" + tail).getBytes();
            out.writeInt(msg.length);
            out.write(msg);
            out.flush();
        } catch (IOException ignored) {}
    }

    private static int roundEven(int v) {
        return (v + 1) & ~1;
    }
}
