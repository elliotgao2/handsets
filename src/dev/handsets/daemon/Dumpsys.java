package dev.handsets.daemon;

import android.os.IBinder;
import android.os.ParcelFileDescriptor;

import java.io.DataOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;

/**
 * dumpsys-equivalent: resolve a registered service from ServiceManager, call
 * {@link IBinder#dump(java.io.FileDescriptor, String[])} into a pipe, and
 * stream the dump output back to the client as chunked frames.
 *
 * Skips the {@code /system/bin/dumpsys} process spawn entirely; per-call
 * latency drops from ~150 ms to a few ms.
 */
final class Dumpsys {

    void dump(String service, String[] args, DataOutputStream out) {
        IBinder b;
        try {
            b = Binders.service(service);
        } catch (Throwable t) {
            errFrame(out, "no-such-service:" + service);
            return;
        }

        ParcelFileDescriptor[] pipe;
        try {
            pipe = ParcelFileDescriptor.createPipe();
        } catch (IOException e) {
            errFrame(out, "pipe-failed:" + e.getMessage());
            return;
        }
        final ParcelFileDescriptor read = pipe[0];
        final ParcelFileDescriptor write = pipe[1];

        // Reader thread streams the pipe's read end out to the client as
        // chunked frames. The dump call on the binder is synchronous; we use
        // a thread so the writer side of the pipe never blocks the binder
        // call on a slow/sluggish socket reader.
        final Thread reader = new Thread(new Runnable() {
            @Override public void run() {
                try (InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(read)) {
                    byte[] buf = new byte[64 * 1024];
                    int n;
                    while ((n = in.read(buf)) > 0) {
                        synchronized (out) {
                            out.writeInt(n);
                            out.write(buf, 0, n);
                        }
                    }
                } catch (IOException ignored) {}
            }
        }, "hs-dumpsys-reader");
        reader.setDaemon(true);
        reader.start();

        try {
            b.dump(write.getFileDescriptor(), args == null ? new String[0] : args);
        } catch (Throwable t) {
            errFrame(out, "dump-threw:" + t.getClass().getSimpleName() + ":" + t.getMessage());
            try { write.close(); } catch (IOException ignored) {}
            try { reader.join(2000); } catch (InterruptedException ignored) {}
            return;
        }
        try { write.close(); } catch (IOException ignored) {}
        try { reader.join(); } catch (InterruptedException ignored) {}
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
