package dev.handsets.daemon;

import android.app.ActivityManager;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;

import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * Direct-Binder am subcommands.
 *
 * `start` and `broadcast` go through the system Context (these wrap the same
 * Binder calls `am` ultimately makes). `force_stop` calls IActivityManager
 * directly via reflection because the public Java API doesn't expose it.
 */
final class Am {

    private final Context ctx;

    Am(Context ctx) { this.ctx = ctx; }

    /**
     * @param component  "pkg/.RelativeClass" or "pkg/fully.Qualified" (required)
     * @param action     intent action, may be null
     * @param data       intent data URI, may be null
     * @param flags      intent flags, OR'd with FLAG_ACTIVITY_NEW_TASK
     */
    byte[] start(String component, String action, String data, int flags) {
        try {
            ComponentName cn = parseComponent(component);
            if (cn == null && component != null && component.indexOf('/') < 0) {
                // Bare package name — resolve the launcher activity the same
                // way `monkey -p pkg 1` and Launcher.startActivity do.
                Intent launch = ctx.getPackageManager().getLaunchIntentForPackage(component);
                if (launch != null) cn = launch.getComponent();
                if (cn == null) {
                    // No package matched. Try the user-visible label as the
                    // last fallback so `hs open "Chrome"` works alongside
                    // `hs open com.android.chrome`.
                    List<android.content.pm.ResolveInfo> hits = findByLabel(component);
                    if (hits.size() == 1) {
                        android.content.pm.ActivityInfo a = hits.get(0).activityInfo;
                        cn = new ComponentName(a.packageName, a.name);
                    } else if (hits.size() > 1) {
                        StringBuilder pkgs = new StringBuilder();
                        for (int i = 0; i < hits.size(); i++) {
                            if (i > 0) pkgs.append(',');
                            pkgs.append(hits.get(i).activityInfo.packageName);
                        }
                        return err("AMBIGUOUS_LABEL:" + component + ":" + pkgs
                                + " — use the package id (e.g. "
                                + hits.get(0).activityInfo.packageName + ")");
                    }
                }
            }
            if (cn == null) return err("bad-component:" + component);
            Intent intent = new Intent();
            intent.setComponent(cn);
            if (action != null && !action.isEmpty()) intent.setAction(action);
            if (data != null && !data.isEmpty()) intent.setData(Uri.parse(data));
            intent.addFlags(flags | Intent.FLAG_ACTIVITY_NEW_TASK);
            // Use IActivityTaskManager.startActivityAsUser (Q+) or
            // IActivityManager.startActivityAsUser (pre-Q). The shell UID is
            // recognised by both as a legitimate caller when callingPackage is
            // "com.android.shell".
            int result = startActivityViaBinder(intent);
            return ok("ok started=" + cn.flattenToShortString() + " result=" + result);
        } catch (java.lang.reflect.InvocationTargetException ite) {
            Throwable cause = ite.getCause() != null ? ite.getCause() : ite;
            return err("start-failed:" + cause.getClass().getSimpleName() + ":" + cause.getMessage());
        } catch (Throwable t) {
            return err("start-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    /**
     * Reflectively call {@code startActivityAsUser} on whichever Binder
     * interface exposes it for our SDK level. We don't pin a specific
     * signature: we walk the methods named "startActivityAsUser" and pick
     * the int-returning overload whose 4th parameter is {@code Intent} —
     * that shape has been stable since API 28.
     */
    private int startActivityViaBinder(Intent intent) throws Exception {
        // Try IActivityTaskManager first (Q+), then IActivityManager.
        Object[] candidates = new Object[] {
                Binders.iActivityTaskManagerOrNull(),
                Binders.iActivityManager(),
        };
        Throwable last = null;
        for (Object svc : candidates) {
            if (svc == null) continue;
            Method m = findStartActivityAsUser(svc.getClass());
            if (m == null) continue;
            try {
                Object[] args = buildStartArgs(m, intent);
                Object ret = m.invoke(svc, args);
                return (ret instanceof Integer) ? (Integer) ret : 0;
            } catch (java.lang.reflect.InvocationTargetException ite) {
                last = ite.getCause() != null ? ite.getCause() : ite;
            }
        }
        if (last instanceof Exception) throw (Exception) last;
        throw new IllegalStateException("no startActivityAsUser binding found");
    }

    private static Method findStartActivityAsUser(Class<?> cls) {
        Method best = null;
        int bestParams = -1;
        for (Method m : cls.getMethods()) {
            if (!"startActivityAsUser".equals(m.getName())) continue;
            if (m.getReturnType() != int.class) continue;
            Class<?>[] p = m.getParameterTypes();
            // Canonical shape: (IApplicationThread, String, [String featureId,] Intent, ...
            // int userId). The 4th param is Intent on Q+; the 3rd on older surfaces.
            int intentIdx = -1;
            for (int i = 0; i < p.length; i++) {
                if (p[i] == Intent.class) { intentIdx = i; break; }
            }
            if (intentIdx < 2 || intentIdx > 3) continue;
            if (p[p.length - 1] != int.class) continue;
            if (p.length > bestParams) {
                best = m;
                bestParams = p.length;
            }
        }
        return best;
    }

    private static Object[] buildStartArgs(Method m, Intent intent) {
        Class<?>[] p = m.getParameterTypes();
        List<Object> args = new ArrayList<>(p.length);
        int intentIdx = -1;
        for (int i = 0; i < p.length; i++) {
            if (p[i] == Intent.class) { intentIdx = i; break; }
        }
        int userId = Binders.myUserId();
        for (int i = 0; i < p.length; i++) {
            if (i == intentIdx) {
                args.add(intent);
            } else if (i == p.length - 1 && p[i] == int.class) {
                args.add(userId);
            } else if (p[i] == String.class) {
                // Treat the first String slot as callingPackage.
                args.add(needsCallingPackage(args) ? "com.android.shell" : null);
            } else if (p[i] == int.class) {
                args.add(0);
            } else {
                args.add(null);
            }
        }
        return args.toArray();
    }

    private static boolean needsCallingPackage(List<Object> argsSoFar) {
        for (Object a : argsSoFar) {
            if ("com.android.shell".equals(a)) return false;
        }
        return true;
    }

    byte[] forceStop(String pkg) {
        try {
            Object iam = Binders.iActivityManager();
            // Newer signature: forceStopPackage(String, int) — the int is userId on every
            // API level we ship to. (Q+ stays at this 2-arg shape.)
            Method m = iam.getClass().getMethod("forceStopPackage", String.class, int.class);
            int userId = Binders.myUserId();
            m.invoke(iam, pkg, userId);
            return ok("ok force-stop=" + pkg);
        } catch (java.lang.reflect.InvocationTargetException ite) {
            Throwable cause = ite.getCause() != null ? ite.getCause() : ite;
            return err("force-stop-failed:" + cause.getClass().getSimpleName() + ":" + cause.getMessage());
        } catch (Throwable t) {
            return err("force-stop-threw:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    byte[] kill(String pkg) {
        try {
            ActivityManager am = (ActivityManager) ctx.getSystemService(Context.ACTIVITY_SERVICE);
            am.killBackgroundProcesses(pkg);
            return ok("ok kill=" + pkg);
        } catch (Throwable t) {
            return err("kill-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    byte[] broadcast(String action, String component, String data) {
        try {
            if ((action == null || action.isEmpty()) && (component == null || component.isEmpty())) {
                return err("broadcast-needs-action-or-component");
            }
            Intent intent = new Intent();
            if (action != null && !action.isEmpty()) intent.setAction(action);
            if (data != null && !data.isEmpty()) intent.setData(Uri.parse(data));
            if (component != null && !component.isEmpty()) {
                ComponentName cn = parseComponent(component);
                if (cn == null) return err("bad-component:" + component);
                intent.setComponent(cn);
            }
            ctx.sendBroadcast(intent);
            return ok("ok broadcast=" + (action != null ? action : component));
        } catch (Throwable t) {
            return err("broadcast-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    /** Launcher activities (ACTION_MAIN + CATEGORY_LAUNCHER) whose app label
     *  matches {@code label} case-insensitively. Restricted to launcher
     *  activities so a label collision with a non-app package — system
     *  providers, overlays — doesn't poison the resolution. */
    private List<android.content.pm.ResolveInfo> findByLabel(String label) {
        List<android.content.pm.ResolveInfo> out = new ArrayList<>();
        if (label == null || label.isEmpty()) return out;
        android.content.pm.PackageManager pm = ctx.getPackageManager();
        Intent probe = new Intent(Intent.ACTION_MAIN);
        probe.addCategory(Intent.CATEGORY_LAUNCHER);
        List<android.content.pm.ResolveInfo> launchers = pm.queryIntentActivities(probe, 0);
        for (android.content.pm.ResolveInfo ri : launchers) {
            try {
                CharSequence l = ri.loadLabel(pm);
                if (l != null && label.equalsIgnoreCase(l.toString())) {
                    out.add(ri);
                }
            } catch (Throwable ignored) {}
        }
        return out;
    }

    /** Accept "pkg/.Class" (relative) or "pkg/full.Class". */
    private static ComponentName parseComponent(String s) {
        if (s == null) return null;
        int slash = s.indexOf('/');
        if (slash <= 0 || slash >= s.length() - 1) return null;
        String pkg = s.substring(0, slash);
        String cls = s.substring(slash + 1);
        if (cls.charAt(0) == '.') cls = pkg + cls;
        else if (cls.indexOf('.') < 0) cls = pkg + "." + cls;
        return new ComponentName(pkg, cls);
    }

    private static byte[] ok(String s) { return s.getBytes(StandardCharsets.UTF_8); }
    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
