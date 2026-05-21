package dev.handsets.daemon;

import android.graphics.Point;
import android.hardware.display.DisplayManager;
import android.view.Display;

import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

/**
 * window-manager helpers — direct binder against IWindowManager. Replaces the
 * {@code adb shell wm size|density|user-rotation} pattern which each spawns
 * a fresh {@code wm} app_process.
 */
final class Wm {

    private final DisplayManager dm;

    Wm(DisplayManager dm) {
        this.dm = dm;
    }

    byte[] info() {
        try {
            Display d = dm.getDisplay(Display.DEFAULT_DISPLAY);
            Point size = new Point();
            d.getRealSize(size);
            android.util.DisplayMetrics dmx = new android.util.DisplayMetrics();
            d.getRealMetrics(dmx);
            int rot = d.getRotation();
            String json = "{\"width\":" + size.x
                    + ",\"height\":" + size.y
                    + ",\"density\":" + dmx.densityDpi
                    + ",\"xdpi\":" + dmx.xdpi
                    + ",\"ydpi\":" + dmx.ydpi
                    + ",\"rotation\":" + rot
                    + "}";
            return json.getBytes(StandardCharsets.UTF_8);
        } catch (Throwable t) {
            return err("wm-info-failed:" + t.getMessage());
        }
    }

    byte[] setRotation(int rotation) {
        try {
            Object iwm = Binders.asInterface(
                    "android.view.IWindowManager$Stub", Binders.service("window"));
            // freezeRotation(int) is the long-stable shape; newer surfaces add
            // a String caller. Try both.
            Method best = null;
            for (Method m : iwm.getClass().getMethods()) {
                if (!"freezeRotation".equals(m.getName())) continue;
                Class<?>[] p = m.getParameterTypes();
                if (p.length >= 1 && p[0] == int.class
                        && (best == null || p.length < best.getParameterTypes().length)) {
                    best = m;
                }
            }
            if (best == null) return err("freezeRotation-missing");
            Class<?>[] p = best.getParameterTypes();
            Object[] args = new Object[p.length];
            args[0] = rotation;
            for (int i = 1; i < p.length; i++) {
                if (p[i] == String.class) args[i] = "com.android.shell";
                else if (p[i] == int.class) args[i] = 0;
                else if (p[i] == boolean.class) args[i] = false;
                else args[i] = null;
            }
            best.invoke(iwm, args);
            return ok("ok rotation=" + rotation);
        } catch (java.lang.reflect.InvocationTargetException ite) {
            Throwable c = ite.getCause() != null ? ite.getCause() : ite;
            return err("rotation-failed:" + c.getClass().getSimpleName() + ":" + c.getMessage());
        } catch (Throwable t) {
            return err("rotation-threw:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    private static byte[] ok(String s) { return s.getBytes(StandardCharsets.UTF_8); }
    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
