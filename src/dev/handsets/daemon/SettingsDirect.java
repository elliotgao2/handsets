package dev.handsets.daemon;

import android.content.Context;
import android.os.Binder;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Process;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

/**
 * Direct settings provider access — skips both the {@code adb shell} hop
 * AND our own {@code /system/bin/settings} exec fallback. Acquires the
 * {@code settings} provider via {@code IActivityManager.getContentProviderExternal},
 * caches the {@code IContentProvider}, then issues {@code provider.call(...)}
 * to {@code GET_/PUT_<ns>}.
 *
 * Expected wire cost: sub-millisecond once warm.
 *
 * Falls back through {@link SettingsApi#execFallback} for any namespace where
 * the reflective binding can't be made.
 */
final class SettingsDirect {

    private final IBinder token = new Binder();
    private volatile Object provider;     // IContentProvider
    private volatile Method callMethod;
    private volatile Object[] callPrefix; // constant args before (method, arg, extras)
    private volatile int authoritySlot = -1;
    private volatile boolean failed;      // sticky: stop retrying if init keeps failing

    /** Get the value for a setting, or null on lookup failure (caller falls back). */
    String tryGet(String ns, String key) {
        if (failed) return null;
        try {
            Object prov = provider();
            if (prov == null) return null;
            int n = callMethod.getParameterTypes().length;
            Bundle extras = new Bundle();
            extras.putInt("_user", Binders.myUserId());
            Object[] args = buildCallArgs(n, "GET_" + ns.toLowerCase(), key, extras);
            Bundle res = (Bundle) callMethod.invoke(prov, args);
            return res == null ? null : res.getString("value");
        } catch (Throwable t) {
            return null;
        }
    }

    /** Try to write a setting; return true on success. */
    boolean tryPut(String ns, String key, String value) {
        if (failed) return false;
        try {
            Object prov = provider();
            if (prov == null) return false;
            int n = callMethod.getParameterTypes().length;
            Bundle extras = new Bundle();
            extras.putString("value", value);
            extras.putInt("_user", Binders.myUserId());
            Object[] args = buildCallArgs(n, "PUT_" + ns.toLowerCase(), key, extras);
            callMethod.invoke(prov, args);
            return true;
        } catch (Throwable t) {
            return false;
        }
    }

    // ---------- lazy init ----------

    private Object provider() {
        Object p = provider;
        if (p != null) return p;
        synchronized (this) {
            if (provider != null) return provider;
            try {
                Object iam = Binders.iActivityManager();
                Method ext = findExternalGetter(iam.getClass());
                if (ext == null) { failed = true; return null; }
                int userId = Binders.myUserId();
                Object holder = ext.invoke(iam, "settings", userId, token, "hs");
                if (holder == null) { failed = true; return null; }
                Field providerField = holder.getClass().getField("provider");
                Object prov = providerField.get(holder);
                if (prov == null) { failed = true; return null; }
                Method call = findCallMethod(prov.getClass());
                if (call == null) { failed = true; return null; }
                Class<?>[] paramTypes = call.getParameterTypes();
                callPrefix = buildPrefix(paramTypes);
                callMethod = call;
                provider = prov;
                return prov;
            } catch (Throwable t) {
                failed = true;
                return null;
            }
        }
    }

    private static Method findExternalGetter(Class<?> iamCls) {
        // Stable shape: (String name, int userId, IBinder token, String tag).
        for (Method m : iamCls.getMethods()) {
            if (!"getContentProviderExternal".equals(m.getName())) continue;
            Class<?>[] p = m.getParameterTypes();
            if (p.length == 4 && p[0] == String.class && p[1] == int.class
                    && IBinder.class.isAssignableFrom(p[2]) && p[3] == String.class) {
                return m;
            }
        }
        return null;
    }

    private Method findCallMethod(Class<?> cls) {
        // Trailing 3 params are always (String method, String arg, Bundle extras);
        // preceding params shift between API levels (callingPkg, featureId,
        // authority, AttributionSource, …). Pick the call() with that suffix.
        Method best = null;
        for (Method m : cls.getMethods()) {
            if (!"call".equals(m.getName())) continue;
            if (m.getReturnType() != Bundle.class) continue;
            Class<?>[] p = m.getParameterTypes();
            if (p.length < 3) continue;
            if (p[p.length - 1] != Bundle.class) continue;
            if (p[p.length - 2] != String.class) continue;
            if (p[p.length - 3] != String.class) continue;
            if (best == null || p.length > best.getParameterTypes().length) best = m;
        }
        return best;
    }

    private Object[] buildPrefix(Class<?>[] paramTypes) {
        int prefixLen = paramTypes.length - 3;
        Object[] pre = new Object[prefixLen];
        Object attributionSource = null;
        boolean seenAttribution = false;
        for (int i = 0; i < prefixLen; i++) {
            Class<?> c = paramTypes[i];
            if (c.getName().endsWith("AttributionSource") && !seenAttribution) {
                if (attributionSource == null) attributionSource = makeAttributionSource();
                pre[i] = attributionSource;
                seenAttribution = true;
            } else if (c == String.class) {
                // Last leftover String slot is the authority; mark it so we
                // know to set it at call time.
                pre[i] = null; // filled at call time if authoritySlot points here
                authoritySlot = i;
            } else {
                pre[i] = null;
            }
        }
        // First String slot of an older signature is callingPkg, not authority.
        // (legacy 4-arg: callingPkg, method, arg, extras — prefixLen=1, slot=0)
        // (5-arg: callingPkg, authority, method, arg, extras — prefixLen=2, slot=1)
        // (5-arg with AttributionSource: AS, authority, method, arg, extras — prefixLen=2, slot=1)
        // For the legacy 4-arg shape there's no authority slot; just callingPkg.
        if (prefixLen == 1 && pre[0] == null) {
            pre[0] = "com.android.shell";
            authoritySlot = -1;
        } else if (prefixLen == 2 && !seenAttribution && pre[0] == null) {
            // (callingPkg, authority, ...)
            pre[0] = "com.android.shell";
            authoritySlot = 1;
        }
        return pre;
    }

    private static Object makeAttributionSource() {
        try {
            Class<?> b = Class.forName("android.content.AttributionSource$Builder");
            Constructor<?> ctor = b.getConstructor(int.class);
            Object builder = ctor.newInstance(Process.myUid());
            Method setPkg = b.getMethod("setPackageName", String.class);
            setPkg.invoke(builder, "com.android.shell");
            return b.getMethod("build").invoke(builder);
        } catch (Throwable t) {
            return null;
        }
    }

    private Object[] buildCallArgs(int paramCount, String method, String arg, Bundle extras) {
        Object[] out = new Object[paramCount];
        System.arraycopy(callPrefix, 0, out, 0, callPrefix.length);
        out[paramCount - 3] = method;
        out[paramCount - 2] = arg;
        out[paramCount - 1] = extras;
        if (authoritySlot >= 0) out[authoritySlot] = "settings";
        return out;
    }

    static byte[] bytes(String s) { return s.getBytes(StandardCharsets.UTF_8); }
}
