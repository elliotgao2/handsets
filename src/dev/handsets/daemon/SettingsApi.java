package dev.handsets.daemon;

import android.content.ContentResolver;
import android.content.Context;
import android.provider.Settings;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;

/**
 * settings get/put — direct ContentResolver against the Settings provider.
 * Skips both the {@code adb shell} hop and the {@code /system/bin/settings}
 * process spawn.
 *
 * Shell UID has WRITE_SECURE_SETTINGS (granted by adbd), so all three
 * namespaces (system / secure / global) are writable from here, same as
 * {@code adb shell settings put …}.
 */
final class SettingsApi {

    private final ContentResolver cr;
    private final SettingsDirect direct = new SettingsDirect();

    SettingsApi(Context ctx) {
        this.cr = ctx.getContentResolver();
    }

    byte[] get(String namespace, String key) {
        // Fast path: direct IContentProvider.call via getContentProviderExternal.
        String v = direct.tryGet(namespace, key);
        if (v != null) return v.getBytes(StandardCharsets.UTF_8);
        // Second try: public ContentResolver helpers. Will usually fail from
        // app_process (no IApplicationThread registration) but is harmless to try.
        try {
            switch (namespace.toLowerCase()) {
                case "system":  v = Settings.System.getString(cr, key); break;
                case "secure":  v = Settings.Secure.getString(cr, key); break;
                case "global":  v = Settings.Global.getString(cr, key); break;
                default: return err("bad-namespace:" + namespace);
            }
            if (v != null) return v.getBytes(StandardCharsets.UTF_8);
        } catch (Throwable ignored) {}
        // Last resort: spawn the settings binary.
        return execFallback("get", namespace, key, null);
    }

    byte[] put(String namespace, String key, String value) {
        if (direct.tryPut(namespace, key, value)) {
            return "ok".getBytes(StandardCharsets.UTF_8);
        }
        try {
            boolean ok;
            switch (namespace.toLowerCase()) {
                case "system":  ok = Settings.System.putString(cr, key, value); break;
                case "secure":  ok = Settings.Secure.putString(cr, key, value); break;
                case "global":  ok = Settings.Global.putString(cr, key, value); break;
                default: return err("bad-namespace:" + namespace);
            }
            if (ok) return "ok".getBytes(StandardCharsets.UTF_8);
        } catch (Throwable ignored) {}
        return execFallback("put", namespace, key, value);
    }

    private byte[] execFallback(String op, String ns, String key, String value) {
        try {
            ProcessBuilder pb;
            if ("get".equals(op)) {
                pb = new ProcessBuilder("/system/bin/settings", "get", ns, key);
            } else {
                pb = new ProcessBuilder("/system/bin/settings", "put", ns, key,
                        value == null ? "" : value);
            }
            pb.redirectErrorStream(true);
            Process p = pb.start();
            StringBuilder sb = new StringBuilder();
            try (BufferedReader r = new BufferedReader(new InputStreamReader(p.getInputStream()))) {
                String line;
                while ((line = r.readLine()) != null) sb.append(line).append('\n');
            }
            int code = p.waitFor();
            String out = sb.toString().trim();
            if (code != 0) return err("settings-exit-" + code + (out.isEmpty() ? "" : ":" + out));
            return ("get".equals(op)
                    ? (out.isEmpty() ? "null" : out)
                    : "ok").getBytes(StandardCharsets.UTF_8);
        } catch (IOException | InterruptedException e) {
            return err("settings-fallback-failed:" + e.getMessage());
        }
    }

    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
