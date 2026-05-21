package dev.handsets.daemon;

import java.io.DataOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * Streams logcat output to the client. The implementation runs the
 * {@code /system/bin/logcat} binary inside our long-lived JVM so we still pay
 * one process fork per stream — but per-line latency drops because we bypass
 * adbd's buffer flush cadence (which is the main contributor to the 100-300 ms
 * tail you see with {@code adb logcat}).
 *
 * Args are passed through verbatim, so the daemon accepts the same flags as
 * the on-device logcat (-d, -v, -s, -T, etc.).
 *
 * Protocol: server writes {@code [len][chunk]} frames as data arrives, then
 * {@code [len=0]} when the underlying process exits (or the streaming socket
 * closes).
 */
final class Logcat {

    void stream(List<String> args, DataOutputStream out) {
        List<String> cmd = new ArrayList<>();
        cmd.add("/system/bin/logcat");
        if (args != null) cmd.addAll(args);

        Process p;
        try {
            p = new ProcessBuilder(cmd).redirectErrorStream(true).start();
        } catch (IOException e) {
            errFrame(out, "logcat-spawn-failed:" + e.getMessage());
            return;
        }

        try (InputStream in = p.getInputStream()) {
            byte[] buf = new byte[64 * 1024];
            int n;
            while ((n = in.read(buf)) > 0) {
                try {
                    synchronized (out) {
                        out.writeInt(n);
                        out.write(buf, 0, n);
                        out.flush();
                    }
                } catch (IOException ioBroken) {
                    break;   // client closed
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
