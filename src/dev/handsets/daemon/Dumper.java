package dev.handsets.daemon;

import android.app.UiAutomation;
import android.graphics.Rect;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;

import java.util.List;

public final class Dumper {

    private final UiAutomation ua;
    private final StringBuilder buf = new StringBuilder(64 * 1024);

    public Dumper(UiAutomation ua) {
        this.ua = ua;
    }

    public synchronized String dumpAll() {
        buf.setLength(0);
        JsonOut out = new JsonOut(buf);
        out.beginObj();
        out.num("ts", System.currentTimeMillis());

        out.key("windows").beginArr();
        List<AccessibilityWindowInfo> windows = ua.getWindows();
        if (windows != null) {
            for (AccessibilityWindowInfo w : windows) {
                writeWindow(w, out);
            }
        }
        out.endArr();
        out.endObj();
        return out.finish();
    }

    public synchronized String dumpActive() {
        buf.setLength(0);
        JsonOut out = new JsonOut(buf);
        out.beginObj();
        out.num("ts", System.currentTimeMillis());

        AccessibilityNodeInfo root = ua.getRootInActiveWindow();
        out.key("root");
        Traverse.writeNode(root, out);

        out.endObj();
        return out.finish();
    }

    private void writeWindow(AccessibilityWindowInfo w, JsonOut out) {
        if (w == null) return;
        out.beginObj();
        out.num("id", w.getId());
        out.num("type", w.getType());
        out.bool("active", w.isActive());
        out.bool("focused", w.isFocused());
        out.num("layer", w.getLayer());

        Rect r = new Rect();
        w.getBoundsInScreen(r);
        out.rect("bounds", r.left, r.top, r.right, r.bottom);

        AccessibilityNodeInfo root = null;
        try {
            root = w.getRoot();
        } catch (Throwable t) {
            // some windows aren't accessible to the shell uid
        }
        out.key("root");
        Traverse.writeNode(root, out);

        out.endObj();
    }
}
