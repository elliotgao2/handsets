package dev.handsets.daemon;

import java.io.DataOutputStream;
import java.io.IOException;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

/**
 * One streaming session over one client socket. Subscribes to a {@link
 * Screenshot.Mirror}'s frame events; each new frame is encoded as JPEG and
 * pushed as a length-prefixed payload over {@code out}. Blocks the calling
 * thread until the client disconnects.
 */
final class Streamer {

    private static final Object WAKE = new Object();

    private final Screenshot.Mirror mirror;
    private final int quality;
    private final int fps;          // 0 = uncapped
    private final DataOutputStream out;

    Streamer(Screenshot.Mirror mirror, int quality, int fps, DataOutputStream out) {
        this.mirror = mirror;
        this.quality = quality;
        this.fps = fps;
        this.out = out;
    }

    /** Upper bound on time between wake checks. Doubles as a keep-alive on
     *  idle screens: we always re-encode and send the cached frame, which
     *  also surfaces a closed socket within this interval. */
    private static final long IDLE_INTERVAL_MS = 500L;

    void serve() {
        // capacity 1 — multiple frame events coalesce into one wake.
        final LinkedBlockingQueue<Object> wake = new LinkedBlockingQueue<>(1);
        Screenshot.FrameListener listener = new Screenshot.FrameListener() {
            @Override public void onFrame() {
                wake.offer(WAKE);
            }
        };
        mirror.subscribe(listener);
        long frameIntervalNs = (fps > 0) ? 1_000_000_000L / fps : 0L;
        long nextDeadlineNs = 0L;

        try {
            // Initial frame so the viewer has something to show before the
            // listener fires for the next source-display change.
            sendOne();
            if (frameIntervalNs > 0) nextDeadlineNs = System.nanoTime() + frameIntervalNs;

            while (true) {
                if (frameIntervalNs > 0) {
                    long waitNs = nextDeadlineNs - System.nanoTime();
                    if (waitNs > 0) {
                        wake.poll(waitNs, TimeUnit.NANOSECONDS);
                    }
                    wake.clear();
                    nextDeadlineNs = System.nanoTime() + frameIntervalNs;
                } else {
                    // Bounded poll so an idle source can't wedge us — the
                    // periodic re-send also detects a closed socket.
                    wake.poll(IDLE_INTERVAL_MS, TimeUnit.MILLISECONDS);
                }
                sendOne();
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

    private void sendOne() throws IOException {
        byte[] jpeg;
        try {
            jpeg = mirror.encodeLatest("jpeg", quality);
        } catch (IllegalStateException noFrame) {
            // Mirror hasn't received a frame yet — skip this tick.
            return;
        }
        out.writeInt(jpeg.length);
        out.write(jpeg);
        out.flush();
    }
}
