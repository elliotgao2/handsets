package dev.handsets.daemon;

import android.system.Os;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;

/**
 * Streaming file pull/push over the daemon's length-prefixed framing.
 *
 * pull protocol:  server writes [len][chunk]* [len=0] (EOF). On failure the
 *                 single response is one frame "ERR:..." then [len=0].
 * push protocol:  client writes [len][chunk]* [len=0], then server writes one
 *                 reply frame "ok" or "ERR:...".
 *
 * Chunk size is 256 KiB; per-frame ceiling enforced by the Server's
 * streamInChunks helper.
 */
final class Files {

    private static final int CHUNK = 256 * 1024;

    void pull(String path, DataOutputStream out) throws IOException {
        File f = new File(path);
        if (!f.exists()) {
            writeFrame(out, errBytes("not-found:" + path));
            out.writeInt(0);
            out.flush();
            return;
        }
        if (f.isDirectory()) {
            writeFrame(out, errBytes("is-directory:" + path));
            out.writeInt(0);
            out.flush();
            return;
        }
        FileInputStream in = null;
        try {
            in = new FileInputStream(f);
            byte[] buf = new byte[CHUNK];
            int n;
            while ((n = in.read(buf)) > 0) {
                out.writeInt(n);
                out.write(buf, 0, n);
            }
            out.writeInt(0);
            out.flush();
        } catch (IOException e) {
            // Best-effort: terminate the stream so the client can recover.
            try { out.writeInt(0); out.flush(); } catch (IOException ignored) {}
            throw e;
        } finally {
            if (in != null) try { in.close(); } catch (IOException ignored) {}
        }
    }

    /**
     * Drain client chunks into {@code path}. Returns the final wire response.
     * Does not write the response itself — the caller (Server) writes one
     * frame containing the returned bytes.
     */
    byte[] push(String path, int mode, long expectedSize, DataInputStream in) {
        File f = new File(path);
        File parent = f.getParentFile();
        if (parent != null && !parent.isDirectory()) {
            // Drain so the socket isn't half-spoken.
            drain(in);
            return errBytes("parent-missing:" + parent);
        }
        FileOutputStream out = null;
        long received = 0L;
        try {
            out = new FileOutputStream(f, false);
            // streamInChunks does the per-frame validation; we get raw bytes here.
            received = copyChunks(in, out);
            out.flush();
            try { out.getFD().sync(); } catch (IOException ignored) {}
            if (mode > 0) {
                try { Os.chmod(path, mode); } catch (Throwable t) {
                    return errBytes("chmod-failed:" + t.getMessage());
                }
            }
            if (expectedSize > 0 && received != expectedSize) {
                return errBytes("size-mismatch:got=" + received + ":want=" + expectedSize);
            }
            return ("ok bytes=" + received).getBytes(StandardCharsets.UTF_8);
        } catch (IOException e) {
            return errBytes("write-failed:" + e.getMessage());
        } finally {
            if (out != null) try { out.close(); } catch (IOException ignored) {}
        }
    }

    /** Read chunked frames from {@code in}, write to {@code out}, return total bytes. */
    static long copyChunks(DataInputStream in, OutputStream out) throws IOException {
        long total = 0L;
        byte[] buf = new byte[CHUNK];
        while (true) {
            int n = in.readInt();
            if (n == 0) return total;
            if (n < 0 || n > 8 * 1024 * 1024) {
                throw new IOException("bad-chunk-size:" + n);
            }
            // Reuse buf if it fits, else allocate.
            byte[] target = (n <= buf.length) ? buf : new byte[n];
            in.readFully(target, 0, n);
            out.write(target, 0, n);
            total += n;
        }
    }

    /** Best-effort discard of an inbound chunked stream after a fatal error. */
    private static void drain(DataInputStream in) {
        try {
            byte[] sink = new byte[CHUNK];
            while (true) {
                int n;
                try { n = in.readInt(); } catch (IOException eof) { return; }
                if (n <= 0) return;
                if (n > 8 * 1024 * 1024) return;
                int remaining = n;
                while (remaining > 0) {
                    int r = in.read(sink, 0, Math.min(sink.length, remaining));
                    if (r < 0) return;
                    remaining -= r;
                }
            }
        } catch (Throwable ignored) {}
    }

    private static void writeFrame(DataOutputStream out, byte[] bytes) throws IOException {
        out.writeInt(bytes.length);
        out.write(bytes);
    }

    private static byte[] errBytes(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
