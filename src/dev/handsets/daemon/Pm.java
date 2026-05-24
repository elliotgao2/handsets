package dev.handsets.daemon;

import android.content.Context;
import android.content.Intent;
import android.content.IntentSender;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;

import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.List;

/**
 * Direct-Binder pm subcommands. Mirrors the surface a developer reaches for
 * after `adb shell pm …` — same calls, no per-command JVM cold-start.
 */
final class Pm {

    private final Context ctx;

    Pm(Context ctx) { this.ctx = ctx; }

    /** Args: optional flags "3" (third-party only) or "s" (system only).
     *  Output columns: package \t label \t sourceDir. Label is the
     *  user-visible app name from the manifest; blank for packages whose
     *  manifest label is just the package name (overlays, RROs, providers). */
    byte[] list(boolean thirdPartyOnly, boolean systemOnly) {
        PackageManager pm = ctx.getPackageManager();
        List<PackageInfo> pkgs = pm.getInstalledPackages(0);
        StringBuilder sb = new StringBuilder(8 * 1024);
        for (PackageInfo p : pkgs) {
            ApplicationInfo ai = p.applicationInfo;
            if (ai == null) continue;
            boolean system = (ai.flags & ApplicationInfo.FLAG_SYSTEM) != 0;
            if (thirdPartyOnly && system) continue;
            if (systemOnly && !system) continue;
            String label = "";
            try {
                CharSequence l = pm.getApplicationLabel(ai);
                if (l != null) {
                    String s = l.toString();
                    if (!s.equals(p.packageName)) {
                        label = s.replace('\t', ' ').replace('\n', ' ');
                    }
                }
            } catch (Throwable ignored) {}
            sb.append(p.packageName)
              .append('\t')
              .append(label)
              .append('\t')
              .append(ai.sourceDir == null ? "" : ai.sourceDir)
              .append('\n');
        }
        return sb.toString().getBytes(StandardCharsets.UTF_8);
    }

    byte[] path(String pkg) {
        try {
            ApplicationInfo ai = ctx.getPackageManager().getApplicationInfo(pkg, 0);
            String src = ai.sourceDir;
            if (src == null) return err("no-sourceDir:" + pkg);
            return src.getBytes(StandardCharsets.UTF_8);
        } catch (PackageManager.NameNotFoundException e) {
            return err("not-found:" + pkg);
        }
    }

    byte[] uninstall(String pkg) {
        try {
            PackageInstaller pi = ctx.getPackageManager().getPackageInstaller();
            LocalIntentSender status = new LocalIntentSender();
            IntentSender sender = status.intentSender();
            pi.uninstall(pkg, sender);
            Intent result = status.await(60_000);
            if (result == null) return err("uninstall-timeout");
            int code = result.getIntExtra(PackageInstaller.EXTRA_STATUS,
                    PackageInstaller.STATUS_FAILURE);
            String msg = result.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
            if (code == PackageInstaller.STATUS_SUCCESS) {
                return ok("ok package=" + pkg);
            }
            return err("uninstall-failed:status=" + code + " msg=" + msg);
        } catch (Throwable t) {
            return err("uninstall-threw:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    byte[] grant(String pkg, String perm) {
        return changeRuntimePerm(pkg, perm, /* grant */ true);
    }

    byte[] revoke(String pkg, String perm) {
        return changeRuntimePerm(pkg, perm, /* grant */ false);
    }

    private byte[] changeRuntimePerm(String pkg, String perm, boolean grant) {
        String methodName = grant ? "grantRuntimePermission" : "revokeRuntimePermission";
        // Permission management moved off IPackageManager onto IPermissionManager
        // around API 30. Try the dedicated service first.
        Object[] candidates = new Object[] {
                Binders.iPermissionManagerOrNull(),
                Binders.iPackageManager(),
        };
        Throwable lastCause = null;
        for (Object svc : candidates) {
            if (svc == null) continue;
            Method picked = findRuntimePermMethod(svc.getClass(), methodName);
            if (picked == null) continue;
            try {
                picked.invoke(svc, buildRuntimePermArgs(picked, pkg, perm));
                return ok("ok " + methodName + " " + pkg + " " + perm);
            } catch (java.lang.reflect.InvocationTargetException ite) {
                lastCause = ite.getCause() != null ? ite.getCause() : ite;
            } catch (Throwable t) {
                lastCause = t;
            }
        }
        if (lastCause != null) {
            return err((grant ? "grant" : "revoke") + "-failed:"
                    + lastCause.getClass().getSimpleName() + ":" + lastCause.getMessage());
        }
        return err(methodName + "-not-exposed");
    }

    private static Method findRuntimePermMethod(Class<?> cls, String name) {
        Method picked = null;
        for (Method m : cls.getMethods()) {
            if (!name.equals(m.getName())) continue;
            Class<?>[] p = m.getParameterTypes();
            if (p.length < 3) continue;
            if (p[0] != String.class || p[1] != String.class) continue;
            if (picked == null || p.length < picked.getParameterTypes().length) {
                picked = m;
            }
        }
        return picked;
    }

    private static Object[] buildRuntimePermArgs(Method m, String pkg, String perm) {
        Class<?>[] p = m.getParameterTypes();
        int userId = Binders.myUserId();
        Object[] args = new Object[p.length];
        args[0] = pkg;
        args[1] = perm;
        boolean userIdAssigned = false;
        for (int i = 2; i < p.length; i++) {
            if (p[i] == int.class && !userIdAssigned) {
                args[i] = userId;
                userIdAssigned = true;
            } else if (p[i] == int.class) {
                args[i] = 0;
            } else if (p[i] == boolean.class) {
                args[i] = false;
            } else {
                args[i] = null;
            }
        }
        return args;
    }

    private static byte[] ok(String s) { return s.getBytes(StandardCharsets.UTF_8); }
    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
