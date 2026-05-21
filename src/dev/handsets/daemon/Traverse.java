package dev.handsets.daemon;

import android.graphics.Rect;
import android.os.Build;
import android.view.accessibility.AccessibilityNodeInfo;

public final class Traverse {

    private static final int PREFETCH = resolvePrefetchFlags();
    private static final Rect TMP_RECT = new Rect();

    public static void writeNode(AccessibilityNodeInfo n, JsonOut out) {
        if (n == null) {
            out.append("null,");
            return;
        }
        out.beginObj();

        CharSequence cls = n.getClassName();
        if (cls != null) out.str("cls", cls);

        CharSequence pkg = n.getPackageName();
        if (pkg != null) out.str("pkg", pkg);

        String rid = n.getViewIdResourceName();
        if (rid != null && !rid.isEmpty()) out.str("rid", rid);

        CharSequence text = n.getText();
        if (text != null && text.length() > 0) out.str("text", text);

        CharSequence desc = n.getContentDescription();
        if (desc != null && desc.length() > 0) out.str("desc", desc);

        if (Build.VERSION.SDK_INT >= 26) {
            CharSequence hint = n.getHintText();
            if (hint != null && hint.length() > 0) out.str("hint", hint);
        }

        Rect b = TMP_RECT;
        n.getBoundsInScreen(b);
        out.rect("bounds", b.left, b.top, b.right, b.bottom);

        StringBuilder flags = new StringBuilder(8);
        if (n.isClickable())       flags.append('c');
        if (n.isLongClickable())   flags.append('L');
        if (n.isScrollable())      flags.append('s');
        if (n.isCheckable())       flags.append('k');
        if (n.isChecked())         flags.append('K');
        if (n.isFocusable())       flags.append('f');
        if (n.isFocused())         flags.append('F');
        if (n.isEnabled())         flags.append('e');
        if (n.isSelected())        flags.append('S');
        if (n.isPassword())        flags.append('p');
        if (n.isVisibleToUser())   flags.append('v');
        if (flags.length() > 0)    out.str("flags", flags);

        int cc = n.getChildCount();
        if (cc > 0) {
            out.key("children").beginArr();
            for (int i = 0; i < cc; i++) {
                AccessibilityNodeInfo child = getChildSafe(n, i);
                writeNode(child, out);
            }
            out.endArr();
        }

        out.endObj();
    }

    private static AccessibilityNodeInfo getChildSafe(AccessibilityNodeInfo n, int i) {
        try {
            if (Build.VERSION.SDK_INT >= 33 && PREFETCH != -1) {
                return n.getChild(i, PREFETCH);
            }
            return n.getChild(i);
        } catch (Throwable t) {
            return null;
        }
    }

    private static int resolvePrefetchFlags() {
        try {
            // FLAG_PREFETCH_DESCENDANTS_HYBRID was added in API 33.
            return AccessibilityNodeInfo.class
                    .getField("FLAG_PREFETCH_DESCENDANTS_HYBRID")
                    .getInt(null);
        } catch (Throwable t) {
            // Older constant (API 16+); takes effect via internal client.
            try {
                return AccessibilityNodeInfo.class
                        .getField("FLAG_PREFETCH_DESCENDANTS")
                        .getInt(null);
            } catch (Throwable t2) {
                return -1;
            }
        }
    }

    private Traverse() {}
}
