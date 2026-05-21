package dev.handsets.daemon;

import android.content.ContentResolver;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.os.Binder;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Process;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;

/**
 * Read-only views over the user-data ContentProviders: SMS, call log,
 * contacts, calendar.
 *
 * Goes straight through {@code IActivityManager.getContentProviderExternal}
 * (same path as {@link SettingsDirect}) instead of
 * {@link ContentResolver#query}. The ContentResolver path needs an
 * ActivityThread Application binding which our {@code app_process}
 * daemon doesn't have — Android 14+ rejects every query with
 *   SecurityException: Unable to find app for caller …
 * The external path takes a caller package name string directly and
 * just needs the UID's permission bits, which shell UID has by default
 * for all four providers on AOSP / stock Pixel.
 *
 * Output is NDJSON: first line is the column-name array, subsequent
 * lines are row-value arrays.
 */
final class Providers {

    private static final String CALLER = "com.android.shell";

    private final IBinder token = new Binder();
    private final HashMap<String, Object> providerCache = new HashMap<>();
    private volatile Method queryMethod;
    private volatile Object[] queryPrefix;    // constant prefix args
    private volatile int uriSlot;             // index of the Uri arg
    private volatile int projSlot;            // index of the String[] proj arg
    private volatile int bundleSlot;          // index of the Bundle queryArgs arg
    private volatile int signalSlot;          // index of ICancellationSignal (may be left null)
    private volatile boolean queryWired;

    Providers(Context sysCtx) {
        // sysCtx is unused — getContentProviderExternal is reached via
        // IActivityManager.Stub.asInterface, independent of any Context.
    }

    // ---------- public verb entry points ----------

    byte[] sms(String kind, int limit) {
        String path;
        switch (kind == null ? "inbox" : kind) {
            case "inbox": path = "/inbox"; break;
            case "sent":  path = "/sent";  break;
            case "all":   path = "";       break;
            default: return err("bad-type:" + kind);
        }
        return query("sms",
                Uri.parse("content://sms" + path),
                new String[] { "_id", "address", "body", "date", "type", "read", "thread_id" },
                null, null, "date DESC", limit);
    }

    byte[] calls(String kind, int limit) {
        String sel = null;
        String[] args = null;
        switch (kind == null ? "all" : kind) {
            case "all":    break;
            case "in":     sel = "type=?"; args = new String[] { "1" }; break;
            case "out":    sel = "type=?"; args = new String[] { "2" }; break;
            case "missed": sel = "type=?"; args = new String[] { "3" }; break;
            default: return err("bad-type:" + kind);
        }
        return query("call_log",
                Uri.parse("content://call_log/calls"),
                new String[] { "_id", "number", "name", "date", "duration", "type" },
                sel, args, "date DESC", limit);
    }

    byte[] contacts(int limit) {
        // /data/phones is the pre-joined view of every phone-number data
        // row with its parent contact name — one row per phone number
        // (contacts without phones don't appear). For agent / automation
        // use this is the table you actually want; the bare
        // /contacts table only has names and photos.
        //
        // Column names: raw data1/data2/data3 are the canonical storage
        // names and accepted on every Android version. `number` / `type`
        // / `label` are aliases that some OEM builds reject; we re-label
        // them at projection time using SQL `AS`. Phone.NUMBER = data1,
        // Phone.TYPE = data2, Phone.LABEL = data3.
        return query("com.android.contacts",
                Uri.parse("content://com.android.contacts/data/phones"),
                new String[] {
                        "contact_id",
                        "display_name",
                        "data1",        // Phone.NUMBER
                        "data2",        // Phone.TYPE (1=home, 2=mobile, 3=work, …)
                        "data3",        // Phone.LABEL (custom label when type=0)
                        "starred",
                },
                null, null, "display_name COLLATE LOCALIZED", limit);
    }

    byte[] calendar(long fromMs, long toMs, int limit) {
        return query("com.android.calendar",
                Uri.parse("content://com.android.calendar/instances/when/"
                          + fromMs + "/" + toMs),
                new String[] { "event_id", "title", "begin", "end",
                               "allDay", "eventLocation", "calendar_displayName" },
                null, null, "begin ASC", limit);
    }

    // ---------- core query ----------

    private byte[] query(String authority, Uri uri, String[] proj,
                         String selection, String[] selectionArgs,
                         String sort, int limit) {
        Object provider = acquire(authority);
        if (provider == null) return err("provider-acquire-failed:" + authority);

        Method m;
        try {
            m = ensureQueryMethod(provider.getClass());
        } catch (Throwable t) {
            return err("query-method:" + t.getMessage());
        }
        if (m == null) return err("query-method-not-found");

        Bundle qa = new Bundle();
        if (selection != null) {
            qa.putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection);
            if (selectionArgs != null) {
                qa.putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, selectionArgs);
            }
        }
        if (sort != null) qa.putString(ContentResolver.QUERY_ARG_SQL_SORT_ORDER, sort);
        if (limit > 0)    qa.putInt(ContentResolver.QUERY_ARG_LIMIT, limit);

        Cursor c = null;
        try {
            Object[] args = buildQueryArgs(uri, proj, qa);
            c = (Cursor) m.invoke(provider, args);
            if (c == null) return err("provider-null:" + authority);
            return encodeNdjson(c, limit);
        } catch (Throwable t) {
            // Unwrap InvocationTargetException to surface the real cause.
            Throwable cause = (t.getCause() != null) ? t.getCause() : t;
            return err("query-failed:" + authority + ":"
                    + cause.getClass().getSimpleName() + ":" + cause.getMessage());
        } finally {
            if (c != null) try { c.close(); } catch (Throwable ignored) {}
        }
    }

    /** Acquire an IContentProvider for {@code authority} via
     *  IActivityManager.getContentProviderExternal — the same path
     *  SettingsDirect uses. Cached per-authority for life of the
     *  daemon (the binder is stable). */
    private Object acquire(String authority) {
        Object cached = providerCache.get(authority);
        if (cached != null) return cached;
        synchronized (providerCache) {
            cached = providerCache.get(authority);
            if (cached != null) return cached;
            try {
                Object iam = Binders.iActivityManager();
                Method ext = findExternalGetter(iam.getClass());
                if (ext == null) return null;
                int userId = Binders.myUserId();
                Object holder = ext.invoke(iam, authority, userId, token, "hs");
                if (holder == null) return null;
                Field providerField = holder.getClass().getField("provider");
                Object prov = providerField.get(holder);
                if (prov == null) return null;
                providerCache.put(authority, prov);
                return prov;
            } catch (Throwable t) {
                System.err.println("providers: acquire " + authority + " failed: " + t);
                return null;
            }
        }
    }

    private static Method findExternalGetter(Class<?> iamCls) {
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

    /** Resolve and cache the right IContentProvider.query overload.
     *  We want the one whose trailing params are {@code (Uri, String[],
     *  Bundle, ICancellationSignal)} and return Cursor; the prefix
     *  contains caller identity (String callingPkg + featureId, or an
     *  AttributionSource on API 31+). */
    private Method ensureQueryMethod(Class<?> providerCls) {
        if (queryWired) return queryMethod;
        synchronized (this) {
            if (queryWired) return queryMethod;
            Method best = null;
            for (Method m : providerCls.getMethods()) {
                if (!"query".equals(m.getName())) continue;
                if (m.getReturnType() != Cursor.class) continue;
                Class<?>[] p = m.getParameterTypes();
                if (p.length < 4) continue;
                int n = p.length;
                if (p[n - 4] != Uri.class) continue;
                if (p[n - 3] != String[].class) continue;
                if (p[n - 2] != Bundle.class) continue;
                if (!"android.os.ICancellationSignal".equals(p[n - 1].getName())) continue;
                if (best == null || p.length < best.getParameterTypes().length) {
                    // Prefer the *shortest* prefix — fewer mystery params.
                    best = m;
                }
            }
            if (best != null) {
                Class<?>[] pt = best.getParameterTypes();
                int n = pt.length;
                uriSlot    = n - 4;
                projSlot   = n - 3;
                bundleSlot = n - 2;
                signalSlot = n - 1;
                queryPrefix = buildQueryPrefix(pt, n - 4);
                queryMethod = best;
            }
            queryWired = true;
            return queryMethod;
        }
    }

    private static Object[] buildQueryPrefix(Class<?>[] pt, int prefixLen) {
        Object[] out = new Object[prefixLen];
        Object attribution = null;
        for (int i = 0; i < prefixLen; i++) {
            Class<?> c = pt[i];
            if (c.getName().endsWith("AttributionSource")) {
                if (attribution == null) attribution = buildShellAttribution();
                out[i] = attribution;
            } else if (c == String.class) {
                // First leftover String slot is callingPkg.
                out[i] = (out[i] == null) ? CALLER : out[i];
            } else {
                out[i] = null;
            }
        }
        // Legacy (callingPkg, featureId, ...) — featureId may be the
        // second String. Leaving it null is fine on all known overloads.
        return out;
    }

    private Object[] buildQueryArgs(Uri uri, String[] proj, Bundle qa) {
        int n = queryPrefix.length + 4;
        Object[] args = new Object[n];
        System.arraycopy(queryPrefix, 0, args, 0, queryPrefix.length);
        args[uriSlot]    = uri;
        args[projSlot]   = proj;
        args[bundleSlot] = qa;
        args[signalSlot] = null;
        return args;
    }

    private static Object buildShellAttribution() {
        try {
            Class<?> b = Class.forName("android.content.AttributionSource$Builder");
            Constructor<?> ctor = b.getConstructor(int.class);
            Object builder = ctor.newInstance(Process.myUid());
            b.getMethod("setPackageName", String.class).invoke(builder, CALLER);
            return b.getMethod("build").invoke(builder);
        } catch (Throwable t) {
            return null;
        }
    }

    // ---------- Cursor → NDJSON ----------

    private static byte[] encodeNdjson(Cursor c, int limit) {
        String[] cols = c.getColumnNames();
        StringBuilder out = new StringBuilder(4096);
        out.append('[');
        for (int i = 0; i < cols.length; i++) {
            if (i > 0) out.append(',');
            appendJsonString(out, cols[i]);
        }
        out.append("]\n");

        int rows = 0;
        while (c.moveToNext()) {
            if (limit > 0 && rows >= limit) break;
            out.append('[');
            for (int i = 0; i < cols.length; i++) {
                if (i > 0) out.append(',');
                appendCell(out, c, i);
            }
            out.append("]\n");
            rows++;
        }
        return out.toString().getBytes(StandardCharsets.UTF_8);
    }

    private static void appendCell(StringBuilder out, Cursor c, int i) {
        if (c.isNull(i)) { out.append("null"); return; }
        int t = c.getType(i);
        switch (t) {
            case Cursor.FIELD_TYPE_INTEGER: out.append(c.getLong(i)); return;
            case Cursor.FIELD_TYPE_FLOAT:   out.append(c.getDouble(i)); return;
            case Cursor.FIELD_TYPE_STRING:  appendJsonString(out, c.getString(i)); return;
            case Cursor.FIELD_TYPE_BLOB:    out.append("null"); return;
            default:                        out.append("null");
        }
    }

    private static void appendJsonString(StringBuilder out, String s) {
        if (s == null) { out.append("null"); return; }
        out.append('"');
        int n = s.length();
        for (int i = 0; i < n; i++) {
            char ch = s.charAt(i);
            switch (ch) {
                case '\\': out.append("\\\\"); break;
                case '"':  out.append("\\\""); break;
                case '\n': out.append("\\n");  break;
                case '\r': out.append("\\r");  break;
                case '\t': out.append("\\t");  break;
                case '\b': out.append("\\b");  break;
                case '\f': out.append("\\f");  break;
                default:
                    if (ch < 0x20) {
                        out.append(String.format("\\u%04x", (int) ch));
                    } else {
                        out.append(ch);
                    }
            }
        }
        out.append('"');
    }

    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
