package dev.handsets.daemon;

import android.content.ClipData;

import java.io.DataOutputStream;
import java.io.IOException;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

/**
 * Clipboard read/write/watch via the {@code IClipboard} binder.
 *
 * AOSP's ClipboardService unconditionally allows shell UID:
 *   {@code if (callingUid == Process.SHELL_UID) return true;}
 * so we don't need foreground-window tricks or runtime grants — just
 * reach the binder and call getPrimaryClip / setPrimaryClip directly.
 *
 * Watch mode polls every {@code interval_ms} (default 500); listener
 * callback wiring would need a custom Binder stub for
 * IOnPrimaryClipChangedListener, which is more code than it's worth
 * given how cheap a clip-read is.
 */
final class Clipboard {

    private static final String CALLER = "com.android.shell";

    private volatile Object svc;
    private volatile Method getClip;
    private volatile Method setClip;

    private synchronized Object svc() {
        if (svc == null) {
            svc = Binders.asInterface(
                    "android.content.IClipboard$Stub",
                    Binders.service("clipboard"));
        }
        return svc;
    }

    // ---------- read ----------

    byte[] get() {
        try {
            Object service = svc();
            Method m = getOrFindGet(service.getClass());
            if (m == null) return err("get-method-not-found");
            ClipData cd = (ClipData) m.invoke(service, buildArgs(m, null));
            if (cd == null) return new byte[0];
            return clipDataToBytes(cd);
        } catch (Throwable t) {
            Throwable c = (t.getCause() != null) ? t.getCause() : t;
            return err("get-failed:" + c.getClass().getSimpleName() + ":" + c.getMessage());
        }
    }

    // ---------- write ----------

    byte[] set(String text) {
        if (text == null) text = "";
        try {
            ClipData cd = ClipData.newPlainText("hs", text);
            Object service = svc();
            Method m = getOrFindSet(service.getClass());
            if (m == null) return err("set-method-not-found");
            m.invoke(service, buildArgs(m, cd));
            return "ok".getBytes(StandardCharsets.UTF_8);
        } catch (Throwable t) {
            Throwable c = (t.getCause() != null) ? t.getCause() : t;
            return err("set-failed:" + c.getClass().getSimpleName() + ":" + c.getMessage());
        }
    }

    // ---------- watch (polled streaming) ----------

    /** Polls the clipboard at {@code intervalMs} cadence. Each time the
     *  text changes, writes one length-prefixed frame with the new
     *  content. Terminates on IO error (client gone) with a 0-length
     *  frame so the host sees a clean end. */
    void watch(DataOutputStream out, long intervalMs) {
        if (intervalMs <= 0) intervalMs = 500;
        String last = currentText();
        // Emit the current value immediately so callers don't have to
        // race the first poll.
        if (last != null) {
            if (!emit(out, last)) return;
        } else {
            last = "";
        }
        while (true) {
            try { Thread.sleep(intervalMs); }
            catch (InterruptedException e) { break; }
            String now = currentText();
            if (now == null) continue;
            if (!now.equals(last)) {
                if (!emit(out, now)) return;
                last = now;
            }
        }
        // Best-effort terminator.
        try {
            synchronized (out) { out.writeInt(0); out.flush(); }
        } catch (IOException ignored) {}
    }

    private String currentText() {
        try {
            Object service = svc();
            Method m = getOrFindGet(service.getClass());
            if (m == null) return null;
            ClipData cd = (ClipData) m.invoke(service, buildArgs(m, null));
            if (cd == null || cd.getItemCount() == 0) return "";
            CharSequence cs = cd.getItemAt(0).getText();
            return cs == null ? "" : cs.toString();
        } catch (Throwable ignored) {
            return null;
        }
    }

    private static boolean emit(DataOutputStream out, String text) {
        byte[] payload = text.getBytes(StandardCharsets.UTF_8);
        try {
            synchronized (out) {
                out.writeInt(payload.length);
                out.write(payload);
                out.flush();
            }
            return true;
        } catch (IOException e) {
            return false;
        }
    }

    // ---------- reflection helpers ----------

    private Method getOrFindGet(Class<?> cls) {
        Method cached = getClip;
        if (cached != null) return cached;
        synchronized (this) {
            if (getClip != null) return getClip;
            for (Method m : cls.getMethods()) {
                if (!"getPrimaryClip".equals(m.getName())) continue;
                if (m.getReturnType() != ClipData.class) continue;
                // Prefer the longest (most-recent) overload.
                if (getClip == null
                        || m.getParameterTypes().length > getClip.getParameterTypes().length) {
                    getClip = m;
                }
            }
            return getClip;
        }
    }

    private Method getOrFindSet(Class<?> cls) {
        Method cached = setClip;
        if (cached != null) return cached;
        synchronized (this) {
            if (setClip != null) return setClip;
            for (Method m : cls.getMethods()) {
                if (!"setPrimaryClip".equals(m.getName())) continue;
                Class<?>[] p = m.getParameterTypes();
                if (p.length < 1 || p[0] != ClipData.class) continue;
                if (setClip == null
                        || p.length > setClip.getParameterTypes().length) {
                    setClip = m;
                }
            }
            return setClip;
        }
    }

    /** Build the arg array for getPrimaryClip / setPrimaryClip. The
     *  signatures across API levels share the same shape:
     *  {@code [ClipData?] String pkg [, String attributionTag] [, int userId] [, int deviceId]} */
    private static Object[] buildArgs(Method m, ClipData clipDataOrNull) {
        Class<?>[] pt = m.getParameterTypes();
        Object[] args = new Object[pt.length];
        int i = 0;
        if (clipDataOrNull != null && pt[0] == ClipData.class) {
            args[i++] = clipDataOrNull;
        }
        boolean stringSlotConsumed = false;
        for (; i < pt.length; i++) {
            Class<?> c = pt[i];
            if (c == String.class) {
                // First String → calling pkg; any later String → attributionTag (null is fine).
                args[i] = stringSlotConsumed ? null : CALLER;
                stringSlotConsumed = true;
            } else if (c == int.class) {
                args[i] = 0;            // userId / deviceId — 0 = current / default
            } else if (c == boolean.class) {
                args[i] = Boolean.FALSE; // e.g. autoSelectionAllowed flag added in newer APIs
            } else if (c == long.class) {
                args[i] = 0L;
            } else {
                args[i] = null;
            }
        }
        return args;
    }

    // ---------- ClipData → bytes ----------

    private static byte[] clipDataToBytes(ClipData cd) {
        if (cd.getItemCount() == 0) return new byte[0];
        ClipData.Item item = cd.getItemAt(0);
        CharSequence cs = item.getText();
        if (cs != null) return cs.toString().getBytes(StandardCharsets.UTF_8);
        // No text item — emit a short note describing what's there.
        StringBuilder sb = new StringBuilder();
        if (item.getUri() != null) sb.append("uri:").append(item.getUri());
        else if (item.getIntent() != null) sb.append("intent:").append(item.getIntent());
        else sb.append("");
        return sb.toString().getBytes(StandardCharsets.UTF_8);
    }

    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
