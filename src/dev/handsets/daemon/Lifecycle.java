package dev.handsets.daemon;

import java.io.DataOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;

/**
 * Streams app-lifecycle events (activity start / resume, app crash, ANR) by
 * tailing the on-device {@code am monitor} command. This is the same data
 * feed {@code adb shell am monitor} produces — we just avoid adbd's buffering
 * (which makes adb monitor unreliable for event-driven tests).
 *
 * Wire: each event line is one frame; stream closes with a 0-length frame.
 *
 * (A more direct path would register an {@code IActivityController} stub via
 * binder, but that requires implementing several AIDL transactions; the
 * {@code am monitor} pipe is good enough until we hit a real bottleneck.)
 */
final class Lifecycle {

    void monitor(DataOutputStream out) {
        Process p;
        try {
            p = new ProcessBuilder("/system/bin/am", "monitor")
                    .redirectErrorStream(true)
                    .start();
        } catch (IOException e) {
            errFrame(out, "monitor-spawn-failed:" + e.getMessage());
            return;
        }

        try (InputStream in = p.getInputStream()) {
            byte[] buf = new byte[16 * 1024];
            int n;
            while ((n = in.read(buf)) > 0) {
                try {
                    synchronized (out) {
                        out.writeInt(n);
                        out.write(buf, 0, n);
                        out.flush();
                    }
                } catch (IOException broken) {
                    break;
                }
            }
        } catch (IOException ignored) {
        } finally {
            try { p.destroy(); } catch (Throwable ignored) {}
        }
        try {
            synchronized (out) { out.writeInt(0); out.flush(); }
        } catch (IOException ignored) {}
    }

    private static void errFrame(DataOutputStream out, String tail) {
        byte[] msg = ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
        try {
            synchronized (out) {
                out.writeInt(msg.length);
                out.write(msg);
                out.writeInt(0);
                out.flush();
            }
        } catch (IOException ignored) {}
    }
}
