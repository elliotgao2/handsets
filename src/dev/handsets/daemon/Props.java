package dev.handsets.daemon;

import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

/**
 * Direct {@code SystemProperties} access. Reaches the same in-process key/value
 * store that {@code /system/bin/getprop} mmaps — but without the per-call
 * process spawn that {@code adb shell getprop} pays.
 *
 * Reads are essentially free. Writes go through {@code __system_property_set},
 * which RPCs into init; only shell-grantable property prefixes succeed.
 */
final class Props {

    private final Method get;
    private final Method set;

    Props() throws Exception {
        Class<?> sp = Class.forName("android.os.SystemProperties");
        this.get = sp.getDeclaredMethod("get", String.class);
        this.set = sp.getDeclaredMethod("set", String.class, String.class);
    }

    byte[] doGet(String key) {
        try {
            Object v = get.invoke(null, key);
            return ((String) v).getBytes(StandardCharsets.UTF_8);
        } catch (Throwable t) {
            return err("getprop-failed:" + t.getMessage());
        }
    }

    byte[] doSet(String key, String value) {
        try {
            set.invoke(null, key, value);
            return "ok".getBytes(StandardCharsets.UTF_8);
        } catch (Throwable t) {
            return err("setprop-failed:" + t.getMessage());
        }
    }

    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
