package dev.handsets.daemon;

import android.content.Context;
import android.os.IBinder;

import java.lang.reflect.Method;

/**
 * Central pool for Binder + system-Context lookups.
 *
 * Everything goes through reflection because we want a single jar to build
 * against the public SDK while still calling hidden APIs at runtime
 * (ServiceManager, IPackageManager$Stub, IActivityManager$Stub).
 *
 * {@link Main#disableHiddenApiRestrictions} runs at process start so all of
 * the reflective lookups below succeed without throwing.
 */
final class Binders {

    private static volatile Context systemContext;

    static synchronized Context systemContext() {
        Context cached = systemContext;
        if (cached != null) return cached;
        try {
            if (android.os.Looper.getMainLooper() == null) {
                android.os.Looper.prepareMainLooper();
            }
            Class<?> at = Class.forName("android.app.ActivityThread");
            Object inst = at.getMethod("systemMain").invoke(null);
            Context ctx = (Context) at.getMethod("getSystemContext").invoke(inst);
            systemContext = ctx;
            return ctx;
        } catch (Throwable t) {
            throw new RuntimeException("system Context lookup failed", t);
        }
    }

    static IBinder service(String name) {
        try {
            Class<?> sm = Class.forName("android.os.ServiceManager");
            Method get = sm.getMethod("getService", String.class);
            IBinder b = (IBinder) get.invoke(null, name);
            if (b == null) throw new IllegalStateException("service '" + name + "' not registered");
            return b;
        } catch (Throwable t) {
            throw new RuntimeException("ServiceManager.getService(" + name + ") failed", t);
        }
    }

    /** Resolve {@code <stubFqcn>.asInterface(IBinder)} and wrap the given binder. */
    static Object asInterface(String stubFqcn, IBinder b) {
        try {
            Class<?> stub = Class.forName(stubFqcn);
            Method m = stub.getMethod("asInterface", IBinder.class);
            return m.invoke(null, b);
        } catch (Throwable t) {
            throw new RuntimeException("asInterface failed for " + stubFqcn, t);
        }
    }

    static Object iPackageManager() {
        return asInterface("android.content.pm.IPackageManager$Stub", service("package"));
    }

    /**
     * Permission management split out from IPackageManager around API 30.
     * Falls back to iPackageManager() so callers can try whichever interface
     * exposes the method they need.
     */
    static Object iPermissionManagerOrNull() {
        try {
            return asInterface(
                    "android.permission.IPermissionManager$Stub", service("permissionmgr"));
        } catch (Throwable t) {
            return null;
        }
    }

    static Object iActivityManager() {
        return asInterface("android.app.IActivityManager$Stub", service("activity"));
    }

    /** {@code UserHandle.myUserId()} is @hide; reach it via reflection. */
    static int myUserId() {
        try {
            Method m = Class.forName("android.os.UserHandle").getMethod("myUserId");
            return (Integer) m.invoke(null);
        } catch (Throwable t) {
            // Decode user-id directly from our UID (userId * 100000 + appId).
            return android.os.Process.myUid() / 100_000;
        }
    }

    /** API 30+. May be unavailable on older surfaces — callers handle null. */
    static Object iActivityTaskManagerOrNull() {
        try {
            return asInterface(
                    "android.app.IActivityTaskManager$Stub", service("activity_task"));
        } catch (Throwable t) {
            return null;
        }
    }

    private Binders() {}
}
