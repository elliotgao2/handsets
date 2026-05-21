package dev.handsets.daemon;

import android.graphics.Bitmap;

import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

/**
 * Tile-based JPEG-diff streamer.
 *
 *   Wire packet (length-prefixed in the existing TCP framing):
 *     u8 type
 *       0x00 (DELTA)    : u16 num_tiles, repeated: (u16 idx, u16 w, u16 h, u32 jlen, bytes)
 *       0x01 (KEYFRAME) : u32 jlen, bytes  (full-frame JPEG)
 *
 *   Picks keyframe when ≥ 50% of tiles changed in one frame, otherwise emits
 *   a delta containing only the changed tiles. Idle keep-alive (every
 *   {@link #IDLE_INTERVAL_MS}) sends a 3-byte empty delta so the viewer
 *   can detect disconnects without burning bandwidth on a static screen.
 */
final class TileStreamer {

    private static final Object WAKE = new Object();
    private static final long IDLE_INTERVAL_MS = 500L;
    private static final int DEFAULT_TILE = 128;
    private static final byte TYPE_DELTA = 0;
    private static final byte TYPE_KEYFRAME = 1;

    private final Screenshot.Mirror mirror;
    private final int quality;
    private final int tileEdge;
    private final DataOutputStream out;

    // Per-stream state, lazily sized in initIfNeeded().
    private int srcW = -1, srcH = -1;
    private int tilesX, tilesY, totalTiles;
    private int[] prevHashes;
    private int[] tilePix;
    private int[] changedIdx;
    private int[] changedW;
    private int[] changedH;

    private final ByteArrayOutputStream packetBuf = new ByteArrayOutputStream(64 * 1024);
    private final ByteArrayOutputStream jpegScratch = new ByteArrayOutputStream(32 * 1024);

    TileStreamer(Screenshot.Mirror mirror, int quality, int tileEdge, DataOutputStream out) {
        this.mirror = mirror;
        this.quality = (quality > 0 && quality <= 100) ? quality : 80;
        this.tileEdge = tileEdge > 0 ? tileEdge : DEFAULT_TILE;
        this.out = out;
    }

    void serve() {
        final LinkedBlockingQueue<Object> wake = new LinkedBlockingQueue<>(1);
        Screenshot.FrameListener listener = new Screenshot.FrameListener() {
            @Override public void onFrame() { wake.offer(WAKE); }
        };
        mirror.subscribe(listener);
        boolean firstFrame = true;
        try {
            while (true) {
                if (!firstFrame) {
                    wake.poll(IDLE_INTERVAL_MS, TimeUnit.MILLISECONDS);
                }
                sendOneFrame(firstFrame);
                firstFrame = false;
            }
        } catch (IOException disconnected) {
            // Normal: client closed the socket.
        } catch (InterruptedException ie) {
            Thread.currentThread().interrupt();
        } catch (Throwable t) {
            t.printStackTrace();
        } finally {
            mirror.unsubscribe(listener);
        }
    }

    private void sendOneFrame(boolean forceKeyframe) throws IOException {
        byte[] body;
        synchronized (mirror.lockObject()) {
            body = buildPacketUnderLock(forceKeyframe);
        }
        if (body == null) return;
        out.writeInt(body.length);
        out.write(body);
        out.flush();
    }

    /** Must be called while holding {@code mirror.lockObject()}. */
    private byte[] buildPacketUnderLock(boolean forceKeyframe) {
        Bitmap bmp = mirror.currentCached();
        if (bmp == null) {
            // No frame yet — send an empty delta so the viewer at least sees
            // liveness.
            return EMPTY_DELTA;
        }
        int dispW = mirror.outW;
        int dispH = mirror.outH;
        initIfNeeded(dispW, dispH);

        // Hash every tile; collect changed.
        int changed = 0;
        for (int row = 0; row < tilesY; row++) {
            int ty = row * tileEdge;
            int th = Math.min(tileEdge, dispH - ty);
            for (int col = 0; col < tilesX; col++) {
                int tx = col * tileEdge;
                int tw = Math.min(tileEdge, dispW - tx);
                bmp.getPixels(tilePix, 0, tw, tx, ty, tw, th);
                int hh = arrayHash(tilePix, tw * th);
                int idx = row * tilesX + col;
                if (forceKeyframe || hh != prevHashes[idx]) {
                    prevHashes[idx] = hh;
                    changedIdx[changed] = idx;
                    changedW[changed] = tw;
                    changedH[changed] = th;
                    changed++;
                }
            }
        }

        boolean keyframe = forceKeyframe || (changed * 2 >= totalTiles);
        packetBuf.reset();
        DataOutputStream dos = new DataOutputStream(packetBuf);
        try {
            if (keyframe) {
                writeKeyframe(dos, bmp, dispW, dispH);
            } else {
                writeDelta(dos, bmp, changed);
            }
        } catch (IOException ioe) {
            // Buffer writes don't really throw IOException, but the contract
            // forces us to handle it.
            return EMPTY_DELTA;
        }
        return packetBuf.toByteArray();
    }

    private void writeKeyframe(DataOutputStream dos, Bitmap bmp, int dispW, int dispH)
            throws IOException {
        dos.writeByte(TYPE_KEYFRAME);
        jpegScratch.reset();
        Bitmap toEncode;
        boolean recycle = false;
        if (bmp.getWidth() == dispW) {
            toEncode = bmp;
        } else {
            // Cached has row-stride padding past the logical display width;
            // crop to the visible area before encoding.
            toEncode = Bitmap.createBitmap(bmp, 0, 0, dispW, dispH);
            recycle = true;
        }
        toEncode.compress(Bitmap.CompressFormat.JPEG, quality, jpegScratch);
        if (recycle) toEncode.recycle();
        byte[] jpeg = jpegScratch.toByteArray();
        dos.writeInt(jpeg.length);
        dos.write(jpeg);
    }

    private void writeDelta(DataOutputStream dos, Bitmap bmp, int changed) throws IOException {
        dos.writeByte(TYPE_DELTA);
        dos.writeShort(changed);
        for (int i = 0; i < changed; i++) {
            int idx = changedIdx[i];
            int tw = changedW[i];
            int th = changedH[i];
            int tx = (idx % tilesX) * tileEdge;
            int ty = (idx / tilesX) * tileEdge;
            Bitmap tileBmp = Bitmap.createBitmap(bmp, tx, ty, tw, th);
            jpegScratch.reset();
            tileBmp.compress(Bitmap.CompressFormat.JPEG, quality, jpegScratch);
            tileBmp.recycle();
            byte[] jbytes = jpegScratch.toByteArray();
            dos.writeShort(idx);
            dos.writeShort(tw);
            dos.writeShort(th);
            dos.writeInt(jbytes.length);
            dos.write(jbytes);
        }
    }

    private void initIfNeeded(int w, int h) {
        if (w == srcW && h == srcH && tilePix != null) return;
        srcW = w;
        srcH = h;
        tilesX = (w + tileEdge - 1) / tileEdge;
        tilesY = (h + tileEdge - 1) / tileEdge;
        totalTiles = tilesX * tilesY;
        prevHashes = new int[totalTiles];
        changedIdx = new int[totalTiles];
        changedW = new int[totalTiles];
        changedH = new int[totalTiles];
        tilePix = new int[tileEdge * tileEdge];
        System.out.println("tilejpeg: stream " + w + "x" + h + " tile=" + tileEdge
                + " (" + tilesX + "x" + tilesY + " = " + totalTiles + " tiles), q=" + quality);
    }

    private static int arrayHash(int[] a, int count) {
        int h = 1;
        for (int i = 0; i < count; i++) h = 31 * h + a[i];
        return h;
    }

    /** Pre-built 3-byte empty-delta packet (type=0, num_tiles=0). */
    private static final byte[] EMPTY_DELTA = new byte[] { TYPE_DELTA, 0, 0 };
}
