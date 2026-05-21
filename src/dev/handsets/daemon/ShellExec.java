package dev.handsets.daemon;

import java.io.DataOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * Generic process passthrough — useful for commands we haven't direct-bindered
 * yet (e.g. anything {@code cmd <service> …}).
 *
 * Compared to {@code adb shell <command>}: we skip adbd's shell-spawn step but
 * still pay the local fork/exec of the requested binary, so the speedup is the
 * adb-side overhead (~50–100 ms cold start on first call, less subsequently).
 *
 * Wire: chunked stdout/stderr stream, then a terminator frame carrying the
 * exit code as decimal text, then a 0-length frame.
 */
final class ShellExec {

    void run(List<String> argv, DataOutputStream out) {
        if (argv == null || argv.isEmpty()) {
            errFrame(out, "shell-needs-argv");
            return;
        }
        Process p;
        try {
            p = new ProcessBuilder(argv).redirectErrorStream(true).start();
        } catch (IOException e) {
            errFrame(out, "spawn-failed:" + e.getMessage());
            return;
        }

        try (InputStream in = p.getInputStream()) {
            byte[] buf = new byte[64 * 1024];
            int n;
            while ((n = in.read(buf)) > 0) {
                synchronized (out) {
                    out.writeInt(n);
                    out.write(buf, 0, n);
                    out.flush();
                }
            }
        } catch (IOException ignored) {}

        int code;
        try { code = p.waitFor(); }
        catch (InterruptedException ie) { code = -1; }

        String exitLine = "__exit__ " + code;
        byte[] exitBytes = exitLine.getBytes(StandardCharsets.UTF_8);
        try {
            synchronized (out) {
                out.writeInt(exitBytes.length);
                out.write(exitBytes);
                out.writeInt(0);
                out.flush();
            }
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
