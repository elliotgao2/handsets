package dev.handsets.daemon;

import android.app.UiAutomation;
import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.PixelFormat;
import android.graphics.Point;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.view.Display;

import java.io.BufferedReader;
import java.io.ByteArrayOutputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * Fast screenshot via a VirtualDisplay that mirrors the default display into
 * an ImageReader at the output resolution. Caller-controlled args:
 *   size    — long-edge in pixels (default 768)
 *   quality — JPEG quality 1..100 (default 80, ignored for PNG)
 *   format  — "jpeg" (default), "webp", or "png"
 *   max=1   — shortcut for native resolution
 *
 * Mirror is recreated on size change (~50 ms one-time cost). Encode-only
 * changes (quality, format) are free.
 */
public final class Screenshot {

    private static final int JPEG_DEFAULT_Q = 80;

    private static final int MAX_CACHED_MIRRORS = 4;

    private final UiAutomation ua;
    private final Point sourceSize;     // (0,0) if lookup failed
    private final DisplayManager displayManager;   // null if framework init failed
    private final boolean mirrorAvailable;

    // Insertion-ordered cache keyed by long-edge resolution.
    // First call for a size pays ~50 ms; subsequent calls are warm.
    private final java.util.LinkedHashMap<Integer, Mirror> mirrors =
            new java.util.LinkedHashMap<Integer, Mirror>(8, 0.75f, true) {
                @Override
                protected boolean removeEldestEntry(java.util.Map.Entry<Integer, Mirror> e) {
                    if (size() > MAX_CACHED_MIRRORS) {
                        try { e.getValue().close(); } catch (Throwable ignored) {}
                        return true;
                    }
                    return false;
                }
            };

    /** Package-private: H264Streamer reuses these to build its own
     *  MediaCodec-backed VirtualDisplay without re-running framework init. */
    DisplayManager displayManager() { return displayManager; }
    Point sourceSize() { return sourceSize; }

    public Screenshot(UiAutomation ua) {
        this.ua = ua;
        this.sourceSize = lookupSourceSize();
        DisplayManager dm = null;
        try {
            dm = initDisplayManager();
        } catch (Throwable t) {
            System.err.println("screenshot: DisplayManager init failed: " + t);
        }
        this.displayManager = dm;

        boolean ok = false;
        if (dm != null) {
            try {
                Mirror m = Mirror.create(dm, sourceSize, 768);
                mirrors.put(768, m);
                ok = true;
                System.out.println("screenshot: mirror " + m.outW + "x" + m.outH
                        + " of display " + sourceSize.x + "x" + sourceSize.y);
            } catch (Throwable t) {
                System.err.println("screenshot: mirror probe failed: " + t);
            }
        }
        this.mirrorAvailable = ok;
    }

    private static DisplayManager initDisplayManager() throws Exception {
        if (android.os.Looper.getMainLooper() == null) {
            android.os.Looper.prepareMainLooper();
        }
        Class<?> atCls = Class.forName("android.app.ActivityThread");
        Object atInst = atCls.getMethod("systemMain").invoke(null);
        Context sysCtx = (Context) atCls.getMethod("getSystemContext").invoke(atInst);
        // Android 14+ rejects createVirtualDisplay when the Context's
        // op-package name doesn't map to the calling UID. The system
        // Context's package is "android" but we run as shell UID (2000),
        // so the check fails with:
        //   SecurityException: packageName must match the calling uid
        // Three-tier strategy:
        //  1. createPackageContext("com.android.shell", 0) — usually
        //     returns a context whose getOpPackageName() is the shell
        //     package, which matches shell UID.
        //  2. If that didn't change getOpPackageName(), wrap the result
        //     to force the override (DisplayManagerGlobal reads
        //     context.getOpPackageName() to populate the system request).
        //  3. If createPackageContext threw, wrap the system context.
        Context base = sysCtx;
        String chosen;
        try {
            base = sysCtx.createPackageContext("com.android.shell", 0);
            chosen = "com.android.shell (pkg-ctx)";
        } catch (Throwable t) {
            System.err.println("screenshot: createPackageContext(com.android.shell) failed: " + t);
            chosen = "com.android.shell (wrapped sysCtx)";
        }
        if (!"com.android.shell".equals(base.getOpPackageName())) {
            final Context inner = base;
            base = new android.content.ContextWrapper(inner) {
                @Override public String getPackageName()   { return "com.android.shell"; }
                @Override public String getOpPackageName() { return "com.android.shell"; }
            };
            chosen += " + wrapper";
        }
        System.err.println("screenshot: display ctx = " + chosen
                + ", op=" + base.getOpPackageName());
        return (DisplayManager) base.getSystemService(Context.DISPLAY_SERVICE);
    }

    public byte[] capture(CaptureArgs args) {
        int longEdge = resolveLongEdge(args);
        int quality = clampQuality(args.quality);
        boolean png = isPng(args.format);

        byte[] result;
        if (mirrorAvailable) {
            try {
                result = mirrorCapture(longEdge, quality, args.format);
            } catch (Throwable t) {
                System.err.println("screenshot: mirror.capture failed, falling back: " + t);
                result = fallbackCapture(longEdge, quality, args.format);
            }
        } else {
            result = fallbackCapture(longEdge, quality, args.format);
        }

        // Optional FLAG_SECURE detection. It shells out to dumpsys, so keep
        // the hot screenshot path pure unless the caller explicitly asks.
        int threshold = png ? 32 * 1024 : 12 * 1024;
        if (args.secureCheck && result.length < threshold) {
            String secureWin = findSecureWindow();
            if (secureWin != null) {
                return ("ERR:secure-window:" + secureWin)
                        .getBytes(StandardCharsets.UTF_8);
            }
        }
        return result;
    }

    /** Walk {@code dumpsys window windows} for the first visible window
     *  with FLAG_SECURE in its {@code fl=} line. Returns the window's
     *  short name (e.g. {@code com.bank.app/.LoginActivity}) or null. */
    private static String findSecureWindow() {
        try {
            Process p = Runtime.getRuntime().exec(
                    new String[] { "dumpsys", "window", "windows" });
            try (BufferedReader r = new BufferedReader(
                    new InputStreamReader(p.getInputStream(),
                            StandardCharsets.UTF_8))) {
                String currentWin = null;
                String line;
                while ((line = r.readLine()) != null) {
                    String t = line.trim();
                    if (t.startsWith("Window #")) {
                        int curly = t.indexOf('{');
                        int closeBrace = t.indexOf('}', curly);
                        if (curly >= 0 && closeBrace > curly) {
                            String inside = t.substring(curly + 1, closeBrace);
                            int sp = inside.lastIndexOf(' ');
                            currentWin = sp >= 0
                                    ? inside.substring(sp + 1)
                                    : inside;
                        }
                    } else if (t.startsWith("fl=") && containsToken(t, "SECURE")) {
                        return currentWin != null ? currentWin : "unknown";
                    }
                }
            }
            p.waitFor();
        } catch (Throwable ignored) {}
        return null;
    }

    /** Whole-token match against a space-separated flags line. */
    private static boolean containsToken(String flagsLine, String token) {
        for (String tok : flagsLine.substring(3).split(" ")) {
            if (token.equals(tok)) return true;
        }
        return false;
    }

    private byte[] mirrorCapture(int longEdge, int quality, String format) throws Exception {
        Mirror m;
        synchronized (mirrors) {
            m = mirrors.get(longEdge);
            if (m == null) {
                m = Mirror.create(displayManager, sourceSize, longEdge);
                mirrors.put(longEdge, m);
            }
        }
        return m.encodeLatest(format, quality);
    }

    private byte[] fallbackCapture(int longEdge, int quality, String format) {
        Bitmap full = null;
        Bitmap scaled = null;
        try {
            full = ua.takeScreenshot();
            if (full == null) return "ERR:screenshot-null".getBytes();
            int currentLong = Math.max(full.getWidth(), full.getHeight());
            float s = (float) longEdge / currentLong;
            if (s < 1f) {
                int tw = Math.max(1, Math.round(full.getWidth() * s));
                int th = Math.max(1, Math.round(full.getHeight() * s));
                scaled = Bitmap.createScaledBitmap(full, tw, th, true);
            }
            Bitmap toEncode = scaled != null ? scaled : full;
            return encode(toEncode, format, quality);
        } catch (Throwable t) {
            return ("ERR:screenshot-failed:" + t.getClass().getSimpleName()
                    + ":" + t.getMessage()).getBytes();
        } finally {
            if (scaled != null) scaled.recycle();
            if (full != null) full.recycle();
        }
    }

    private static byte[] encode(Bitmap bmp, String format, int quality) {
        ByteArrayOutputStream baos = new ByteArrayOutputStream(64 * 1024);
        Bitmap.CompressFormat cf = compressFormat(format);
        bmp.compress(cf, cf == Bitmap.CompressFormat.PNG ? 100 : quality, baos);
        return baos.toByteArray();
    }

    private static boolean isPng(String format) {
        return "png".equalsIgnoreCase(format);
    }

    private static Bitmap.CompressFormat compressFormat(String format) {
        if (isPng(format)) return Bitmap.CompressFormat.PNG;
        if ("webp".equalsIgnoreCase(format)) {
            try {
                return Bitmap.CompressFormat.valueOf("WEBP_LOSSY");
            } catch (IllegalArgumentException ignored) {
                return Bitmap.CompressFormat.valueOf("WEBP");
            }
        }
        return Bitmap.CompressFormat.JPEG;
    }

    private int resolveLongEdge(CaptureArgs args) {
        if (args.max) return Math.max(sourceSize.x, sourceSize.y);
        int v = args.longEdge > 0 ? args.longEdge : 768;
        if (sourceSize.x > 0) {
            int srcLong = Math.max(sourceSize.x, sourceSize.y);
            if (v > srcLong) v = srcLong;
        }
        return v;
    }

    private static int clampQuality(int q) {
        if (q <= 0) return JPEG_DEFAULT_Q;
        if (q > 100) return 100;
        return q;
    }

    private static Point lookupSourceSize() {
        try {
            Class<?> dmg = Class.forName("android.hardware.display.DisplayManagerGlobal");
            Object inst = dmg.getMethod("getInstance").invoke(null);
            Display d = (Display) dmg.getMethod("getRealDisplay", int.class)
                    .invoke(inst, Display.DEFAULT_DISPLAY);
            Point p = new Point();
            d.getRealSize(p);
            return p;
        } catch (Throwable t) {
            return new Point(0, 0);
        }
    }

    // ---- value object ----

    public static final class CaptureArgs {
        public int longEdge = 0;        // 0 means "default" (768)
        public int quality = JPEG_DEFAULT_Q;
        public String format = "jpeg";
        public boolean max = false;
        public int fps = 0;             // 0 = uncapped (stream command only)
        public int bitrateKbps = 0;     // 0 = default (6000 for stream_h264)
        public int tile = 0;            // 0 = default (128 for stream_tilejpeg)
        public int gopSec = 0;          // 0 = default (1 for stream_h264)
        public boolean secureCheck = false;
    }

    // ---- streaming hook ----

    /** Notified on the mirror's listener thread whenever a fresh frame has
     *  landed in {@code cached}. Implementations MUST be non-blocking. */
    public interface FrameListener {
        void onFrame();
    }

    /**
     * Returns the (possibly-newly-created) mirror for the resolution implied
     * by {@code args}. Used by the stream command to subscribe to frame events
     * on the same cached mirror the per-call screenshot path uses.
     */
    public Mirror mirrorFor(CaptureArgs args) throws Exception {
        if (!mirrorAvailable) throw new IllegalStateException("mirror unavailable");
        int longEdge = resolveLongEdge(args);
        synchronized (mirrors) {
            Mirror m = mirrors.get(longEdge);
            if (m == null) {
                m = Mirror.create(displayManager, sourceSize, longEdge);
                mirrors.put(longEdge, m);
            }
            return m;
        }
    }

    // ---- mirror pipeline ----

    static final class Mirror {
        final ImageReader reader;
        final VirtualDisplay vd;
        final HandlerThread listenerThread;
        final int outW, outH;
        final Object lock = new Object();
        // COW so fanout doesn't need the lock and listeners can come and go safely.
        private final CopyOnWriteArrayList<FrameListener> subscribers = new CopyOnWriteArrayList<>();

        void subscribe(FrameListener l) { subscribers.add(l); }
        void unsubscribe(FrameListener l) { subscribers.remove(l); }

        /** Lock guarding {@code cached}. Hold during pixel reads. */
        Object lockObject() { return lock; }

        /** Most-recent fully-written cached bitmap. Only safe to use while
         *  holding {@link #lockObject()}. */
        Bitmap currentCached() { return cached; }

        /** Pixel width of {@link #currentCached()} (may exceed outW by row-stride padding). */
        int cachedStridePix() { return cachedRowPix; }

        // The listener thread does the expensive GPU-fence-blocking
        // copyPixelsFromBuffer into `scratch` *without* holding the lock, then
        // briefly takes the lock just to swap scratch <-> cached. Capture
        // threads only ever read `cached` under the lock and never wait on
        // GPU work.
        private Bitmap cached;        // most recent stable frame at stride width
        private int cachedRowPix;
        private Bitmap scratch;       // owned by listener thread; swapped in & out
        private int scratchRowPix;

        private Mirror(ImageReader reader, VirtualDisplay vd, HandlerThread listenerThread,
                       int outW, int outH) {
            this.reader = reader;
            this.vd = vd;
            this.listenerThread = listenerThread;
            this.outW = outW;
            this.outH = outH;
        }

        static Mirror create(DisplayManager dm, Point sourceSize, int targetLongEdge)
                throws Exception {
            int srcW = sourceSize.x, srcH = sourceSize.y;
            if (srcW <= 0 || srcH <= 0) {
                Display d = dm.getDisplay(Display.DEFAULT_DISPLAY);
                Point p = new Point();
                d.getRealSize(p);
                srcW = p.x; srcH = p.y;
            }

            int longEdge = Math.max(srcW, srcH);
            float scale = (float) targetLongEdge / longEdge;
            if (scale > 1f) scale = 1f;
            int outW = Math.max(1, Math.round(srcW * scale));
            int outH = Math.max(1, Math.round(srcH * scale));

            ImageReader reader = ImageReader.newInstance(outW, outH, PixelFormat.RGBA_8888, 3);

            int flags;
            try {
                flags = DisplayManager.class.getDeclaredField("VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR").getInt(null);
            } catch (Throwable t) {
                flags = 16;
            }

            HandlerThread ht = new HandlerThread("hs-mirror-listener-" + outW);
            ht.start();
            Handler handler = new Handler(ht.getLooper());

            // Build the mirror shell so we can register the listener before
            // the VirtualDisplay starts producing frames.
            final Mirror[] holder = new Mirror[1];
            reader.setOnImageAvailableListener(new ImageReader.OnImageAvailableListener() {
                @Override public void onImageAvailable(ImageReader r) {
                    Mirror mr = holder[0];
                    if (mr == null) return;
                    Image img;
                    try { img = r.acquireLatestImage(); } catch (Throwable t) { return; }
                    if (img == null) return;
                    try { mr.absorb(img); }
                    catch (Throwable ignored) {}
                    finally { img.close(); }
                }
            }, handler);

            VirtualDisplay vd = dm.createVirtualDisplay(
                    "hs-mirror-" + outW + "x" + outH, outW, outH, 320,
                    reader.getSurface(), flags);
            if (vd == null) {
                reader.close();
                ht.quitSafely();
                throw new RuntimeException("createVirtualDisplay returned null");
            }

            Mirror m = new Mirror(reader, vd, ht, outW, outH);
            holder[0] = m;

            // Wait up to 1500 ms for the first frame so the very first capture
            // has data. On an idle source the compositor may delay the initial
            // mirror frame; the listener still wakes us as soon as it arrives.
            long deadline = System.nanoTime() + 1_500_000_000L;
            while (System.nanoTime() < deadline) {
                synchronized (m.lock) {
                    if (m.cached != null) return m;
                }
                // Also try a direct pull — sometimes the listener doesn't fire
                // for the very first frame but the queue has it.
                Image img = reader.acquireLatestImage();
                if (img != null) {
                    try { m.absorb(img); } finally { img.close(); }
                    return m;
                }
                Thread.sleep(15);
            }
            return m;
        }

        byte[] encodeLatest(String format, int quality) {
            synchronized (lock) {
                if (cached == null) throw new IllegalStateException("no frame yet");
                Bitmap toEncode;
                boolean recycle = false;
                if (cachedRowPix == outW) {
                    toEncode = cached;
                } else {
                    toEncode = Bitmap.createBitmap(cached, 0, 0, outW, outH);
                    recycle = true;
                }
                try {
                    ByteArrayOutputStream baos = new ByteArrayOutputStream(64 * 1024);
                    Bitmap.CompressFormat cf = compressFormat(format);
                    toEncode.compress(cf, cf == Bitmap.CompressFormat.PNG ? 100 : quality, baos);
                    return baos.toByteArray();
                } finally {
                    if (recycle) toEncode.recycle();
                }
            }
        }

        private void absorb(Image img) {
            Image.Plane p = img.getPlanes()[0];
            int stride = p.getRowStride();
            int pixelStride = p.getPixelStride();
            int rowPix = stride / Math.max(1, pixelStride);
            int h = img.getHeight();

            // Allocate / resize the listener's private scratch outside the lock.
            if (scratch == null || scratchRowPix != rowPix || scratch.getHeight() != h) {
                if (scratch != null) scratch.recycle();
                scratch = Bitmap.createBitmap(rowPix, h, Bitmap.Config.ARGB_8888);
                scratchRowPix = rowPix;
            }
            // This is the GPU-fence-blocking call (~16 ms on a vsync-locked
            // producer). Keep it outside the lock.
            scratch.copyPixelsFromBuffer(p.getBuffer());

            synchronized (lock) {
                Bitmap oldCached = cached;
                int oldRowPix = cachedRowPix;
                cached = scratch;
                cachedRowPix = scratchRowPix;
                scratch = oldCached;       // reuse previous cached as next scratch
                scratchRowPix = oldRowPix;
            }

            // Fan out to subscribers outside the lock. COW makes iteration safe.
            // Listeners must be non-blocking.
            if (!subscribers.isEmpty()) {
                for (FrameListener l : subscribers) {
                    try { l.onFrame(); } catch (Throwable ignored) {}
                }
            }
        }

        void close() {
            try { reader.setOnImageAvailableListener(null, null); } catch (Throwable ignored) {}
            try { vd.release(); } catch (Throwable ignored) {}
            try { reader.close(); } catch (Throwable ignored) {}
            try { listenerThread.quitSafely(); } catch (Throwable ignored) {}
            synchronized (lock) {
                if (cached != null) { cached.recycle(); cached = null; }
                if (scratch != null) { scratch.recycle(); scratch = null; }
            }
        }
    }
}
