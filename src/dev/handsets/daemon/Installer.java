package dev.handsets.daemon;

import android.content.Context;
import android.content.Intent;
import android.content.IntentSender;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;

import java.io.DataInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

/**
 * Stream-install an APK via {@link PackageInstaller}, the same path
 * {@code adb install} uses internally. We skip the /data/local/tmp staging
 * copy by writing chunked frames straight into {@code session.openWrite}.
 *
 * Install result is reported back via a local {@link Binder} that implements
 * just the {@code IIntentSender.send} transaction (transaction code 1, stable
 * across all known API levels). That dodges the BroadcastReceiver dance that
 * would require a proper package name for our app_process daemon.
 */
final class Installer {

    private final Context ctx;

    Installer(Context ctx) { this.ctx = ctx; }

    /**
     * @param size       APK byte length (0 = unknown; we still bound the copy by EOF)
     * @param reinstall  set INSTALL_REPLACE_EXISTING
     * @param grantAll   grant runtime perms after commit (per-permission, not the
     *                   INSTALL_GRANT_ALL flag — that flag isn't shell-grantable
     *                   on every API level)
     * @param in         length-prefixed chunked frames; terminated by len=0
     */
    /**
     * Streaming split-APK install. Server already knows how many APKs to read
     * and their sizes from the command frame.
     */
    byte[] installMulti(long[] sizes, boolean reinstall, boolean grantAll, DataInputStream in) {
        if (sizes == null || sizes.length == 0) {
            drain(in); return err("install-multi-no-sizes");
        }
        PackageInstaller pi = ctx.getPackageManager().getPackageInstaller();
        PackageInstaller.SessionParams params =
                new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
        long total = 0; for (long s : sizes) total += s;
        if (total > 0) params.setSize(total);
        if (reinstall) trySetInstallFlags(params, 0x2);
        trySetRequireUserAction(params, 2);

        int sessionId;
        PackageInstaller.Session session = null;
        try {
            sessionId = pi.createSession(params);
        } catch (IOException e) {
            for (long ignored : sizes) drain(in);
            return err("createSession-failed:" + e.getMessage());
        }

        long written = 0;
        try {
            session = pi.openSession(sessionId);
            for (int i = 0; i < sizes.length; i++) {
                String slot = (i == 0 ? "base.apk" : ("split_" + i + ".apk"));
                OutputStream apk = session.openWrite(slot, 0, sizes[i] > 0 ? sizes[i] : -1);
                long got;
                try { got = Files.copyChunks(in, apk); session.fsync(apk); }
                finally { try { apk.close(); } catch (IOException ignored) {} }
                if (sizes[i] > 0 && got != sizes[i]) {
                    session.abandon();
                    return err("split-" + i + "-size-mismatch:got=" + got + ":want=" + sizes[i]);
                }
                written += got;
            }

            LocalIntentSender status = new LocalIntentSender();
            session.commit(status.intentSender());
            Intent result = status.await(180_000);
            if (result == null) return err("install-timeout");
            int code = result.getIntExtra(PackageInstaller.EXTRA_STATUS,
                    PackageInstaller.STATUS_FAILURE);
            String pkg = result.getStringExtra(PackageInstaller.EXTRA_PACKAGE_NAME);
            String msg = result.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
            if (code == PackageInstaller.STATUS_SUCCESS) {
                if (grantAll && pkg != null) grantAllRequested(pkg);
                return ok("ok package=" + (pkg != null ? pkg : "?")
                        + " splits=" + sizes.length + " bytes=" + written);
            }
            return err("install-failed:status=" + code + " pkg=" + pkg + " msg=" + msg);
        } catch (Throwable t) {
            try { if (session != null) session.abandon(); } catch (Throwable ignored) {}
            return err("install-multi-threw:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        } finally {
            if (session != null) try { session.close(); } catch (Throwable ignored) {}
        }
    }

    byte[] install(long size, boolean reinstall, boolean grantAll, DataInputStream in) {
        PackageInstaller pi = ctx.getPackageManager().getPackageInstaller();
        PackageInstaller.SessionParams params =
                new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
        if (size > 0) params.setSize(size);
        if (reinstall) trySetInstallFlags(params, /* INSTALL_REPLACE_EXISTING */ 0x2);
        // setRequireUserAction(USER_ACTION_NOT_REQUIRED=2) on API 31+; ignored
        // pre-31 (shell-side installs already skip the UI on older versions).
        trySetRequireUserAction(params, 2);

        int sessionId;
        PackageInstaller.Session session = null;
        try {
            sessionId = pi.createSession(params);
        } catch (IOException e) {
            // Drain the inbound stream so the socket stays in sync.
            drain(in);
            return err("createSession-failed:" + e.getMessage());
        }

        try {
            session = pi.openSession(sessionId);
            OutputStream apk = session.openWrite("base.apk", 0, size > 0 ? size : -1);
            long written;
            try {
                written = Files.copyChunks(in, apk);
                session.fsync(apk);
            } finally {
                try { apk.close(); } catch (IOException ignored) {}
            }
            if (size > 0 && written != size) {
                session.abandon();
                return err("size-mismatch:got=" + written + ":want=" + size);
            }

            LocalIntentSender status = new LocalIntentSender();
            IntentSender sender = status.intentSender();
            session.commit(sender);

            Intent result = status.await(120_000);
            if (result == null) return err("install-timeout");

            int code = result.getIntExtra(PackageInstaller.EXTRA_STATUS,
                    PackageInstaller.STATUS_FAILURE);
            String pkg = result.getStringExtra(PackageInstaller.EXTRA_PACKAGE_NAME);
            String msg = result.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);

            if (code == PackageInstaller.STATUS_SUCCESS) {
                if (grantAll && pkg != null) grantAllRequested(pkg);
                return ok("ok package=" + (pkg != null ? pkg : "?") + " bytes=" + written);
            }
            return err("install-failed:status=" + code + " pkg=" + pkg + " msg=" + msg);
        } catch (Throwable t) {
            try { if (session != null) session.abandon(); } catch (Throwable ignored) {}
            return err("install-threw:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        } finally {
            if (session != null) try { session.close(); } catch (Throwable ignored) {}
        }
    }

    private static void drain(DataInputStream in) {
        try {
            byte[] sink = new byte[256 * 1024];
            while (true) {
                int n;
                try { n = in.readInt(); } catch (IOException eof) { return; }
                if (n <= 0 || n > 8 * 1024 * 1024) return;
                int remaining = n;
                while (remaining > 0) {
                    int r = in.read(sink, 0, Math.min(sink.length, remaining));
                    if (r < 0) return;
                    remaining -= r;
                }
            }
        } catch (Throwable ignored) {}
    }

    private static void trySetInstallFlags(PackageInstaller.SessionParams params, int flag) {
        try {
            Method m = PackageInstaller.SessionParams.class
                    .getDeclaredMethod("setInstallFlags", int.class);
            m.setAccessible(true);
            // setInstallFlags replaces; OR in our flag with whatever was there.
            java.lang.reflect.Field f =
                    PackageInstaller.SessionParams.class.getDeclaredField("installFlags");
            f.setAccessible(true);
            int prev = f.getInt(params);
            m.invoke(params, prev | flag);
        } catch (Throwable t) {
            // Fallback: write the field directly.
            try {
                java.lang.reflect.Field f =
                        PackageInstaller.SessionParams.class.getDeclaredField("installFlags");
                f.setAccessible(true);
                f.setInt(params, f.getInt(params) | flag);
            } catch (Throwable ignored) {}
        }
    }

    private static void trySetRequireUserAction(PackageInstaller.SessionParams params, int mode) {
        try {
            Method m = PackageInstaller.SessionParams.class
                    .getDeclaredMethod("setRequireUserAction", int.class);
            m.invoke(params, mode);
        } catch (Throwable ignored) {
            // pre-API-31; default behaviour is fine.
        }
    }

    private void grantAllRequested(String pkg) {
        try {
            PackageManager pm = ctx.getPackageManager();
            String[] requested = pm.getPackageInfo(pkg, PackageManager.GET_PERMISSIONS)
                    .requestedPermissions;
            if (requested == null) return;
            Object[] svcs = new Object[] {
                    Binders.iPermissionManagerOrNull(),
                    Binders.iPackageManager(),
            };
            Object svc = null;
            Method grant = null;
            for (Object s : svcs) {
                if (s == null) continue;
                Method m = findGrant(s.getClass());
                if (m != null) { svc = s; grant = m; break; }
            }
            if (grant == null) return;
            Class<?>[] gp = grant.getParameterTypes();
            int userId = Binders.myUserId();
            for (String perm : requested) {
                Object[] args = new Object[gp.length];
                args[0] = pkg; args[1] = perm;
                boolean uidSet = false;
                for (int i = 2; i < gp.length; i++) {
                    if (gp[i] == int.class && !uidSet) { args[i] = userId; uidSet = true; }
                    else if (gp[i] == int.class) args[i] = 0;
                    else if (gp[i] == boolean.class) args[i] = false;
                    else args[i] = null;
                }
                try { grant.invoke(svc, args); }
                catch (Throwable ignored) { /* non-runtime perms throw — that's fine */ }
            }
        } catch (Throwable t) {
            System.err.println("warn: grantAllRequested failed: " + t);
        }
    }

    private static Method findGrant(Class<?> cls) {
        Method picked = null;
        for (Method m : cls.getMethods()) {
            if (!"grantRuntimePermission".equals(m.getName())) continue;
            Class<?>[] p = m.getParameterTypes();
            if (p.length >= 3 && p[0] == String.class && p[1] == String.class
                    && (picked == null || p.length < picked.getParameterTypes().length)) {
                picked = m;
            }
        }
        return picked;
    }

    private static byte[] ok(String s) { return s.getBytes(StandardCharsets.UTF_8); }
    private static byte[] err(String tail) { return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8); }
}
