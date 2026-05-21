package dev.handsets.daemon;

import android.app.UiAutomation;
import android.os.Bundle;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;

import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.List;

/**
 * Accessibility-action shortcuts. Lets RPA scripts do things like
 *   node_click text="Sign in"
 *   node_set_text id=com.foo:id/email text="user@example.com"
 *   node_scroll class=androidx.recyclerview.widget.RecyclerView dir=forward
 *
 * Faster and more reliable than coordinate gestures: no virtual-keyboard
 * round-trip, no race with layout animations, no chance of an overlapping
 * window swallowing the tap.
 */
final class NodeActions {

    private final UiAutomation ua;

    NodeActions(UiAutomation ua) { this.ua = ua; }

    // ---------- public action wrappers ----------

    byte[] click(String selectorStr) {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_CLICK, null, "click");
    }

    byte[] longClick(String selectorStr) {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_LONG_CLICK, null, "long_click");
    }

    byte[] focus(String selectorStr) {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_FOCUS, null, "focus");
    }

    byte[] setText(String selectorStr, String text) {
        Bundle args = new Bundle();
        args.putCharSequence(
                AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text);
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_SET_TEXT, args, "set_text");
    }

    byte[] scroll(String selectorStr, String dir) {
        int action = "backward".equalsIgnoreCase(dir)
                ? AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
                : AccessibilityNodeInfo.ACTION_SCROLL_FORWARD;
        return perform(selectorStr, action, null, "scroll_" + dir);
    }

    // ---------- selector-based finder (also reusable by wait_for_text) ----------

    Selector parseSelector(String s) { return Selector.parse(s); }

    /** Locate the first node matching {@code sel}, or null. */
    AccessibilityNodeInfo find(Selector sel) {
        List<AccessibilityWindowInfo> windows = ua.getWindows();
        if (windows == null || windows.isEmpty()) {
            AccessibilityNodeInfo root = ua.getRootInActiveWindow();
            return root == null ? null : bfs(root, sel);
        }
        for (AccessibilityWindowInfo w : windows) {
            AccessibilityNodeInfo root = w.getRoot();
            if (root == null) continue;
            AccessibilityNodeInfo hit = bfs(root, sel);
            if (hit != null) return hit;
        }
        return null;
    }

    private static AccessibilityNodeInfo bfs(AccessibilityNodeInfo root, Selector sel) {
        Deque<AccessibilityNodeInfo> q = new ArrayDeque<>();
        q.add(root);
        while (!q.isEmpty()) {
            AccessibilityNodeInfo n = q.removeFirst();
            if (n == null) continue;
            if (sel.matches(n)) return n;
            int kids = n.getChildCount();
            for (int i = 0; i < kids; i++) {
                AccessibilityNodeInfo c = n.getChild(i);
                if (c != null) q.add(c);
            }
        }
        return null;
    }

    private byte[] perform(String selectorStr, int action, Bundle args, String opName) {
        Selector sel;
        try { sel = Selector.parse(selectorStr); }
        catch (IllegalArgumentException e) { return err("bad-selector:" + e.getMessage()); }

        AccessibilityNodeInfo n = find(sel);
        if (n == null) return err("not-found:" + sel);
        boolean ok;
        try { ok = n.performAction(action, args); }
        catch (Throwable t) { return err(opName + "-threw:" + t.getMessage()); }
        return ok ? ok("ok " + opName) : err(opName + "-rejected");
    }

    // ---------- selector model ----------

    /** Conjunction of attribute predicates: {@code text="x" id="y"}. */
    static final class Selector {
        String textExact, textSub;
        String descExact, descSub;
        String idExact;
        String classExact;
        String pkgExact;

        static Selector parse(String s) {
            if (s == null || s.isEmpty()) {
                throw new IllegalArgumentException("empty");
            }
            Selector sel = new Selector();
            // Split on whitespace but respect quoted values.
            List<String> toks = tokenize(s);
            for (String tok : toks) {
                int eq = tok.indexOf('=');
                if (eq <= 0) {
                    // Bare token = CSS-like class shorthand:
                    //   `EditText` → class=EditText (simple-name matched below).
                    sel.classExact = unquote(tok);
                    continue;
                }
                boolean sub = tok.charAt(eq - 1) == '~';
                int keyEnd = sub ? eq - 1 : eq;
                String k = tok.substring(0, keyEnd);
                String v = unquote(tok.substring(eq + 1));
                switch (k) {
                    case "text":  if (sub) sel.textSub = v; else sel.textExact = v; break;
                    case "desc":  if (sub) sel.descSub = v; else sel.descExact = v; break;
                    case "id":    sel.idExact = v; break;
                    case "class": sel.classExact = v; break;
                    case "pkg":   sel.pkgExact = v; break;
                    default: throw new IllegalArgumentException("unknown selector key: " + k);
                }
            }
            return sel;
        }

        boolean matches(AccessibilityNodeInfo n) {
            if (textExact != null) {
                CharSequence t = n.getText();
                if (t == null || !textExact.contentEquals(t)) return false;
            }
            if (textSub != null) {
                CharSequence t = n.getText();
                if (t == null || !t.toString().contains(textSub)) return false;
            }
            if (descExact != null) {
                CharSequence d = n.getContentDescription();
                if (d == null || !descExact.contentEquals(d)) return false;
            }
            if (descSub != null) {
                CharSequence d = n.getContentDescription();
                if (d == null || !d.toString().contains(descSub)) return false;
            }
            if (idExact != null) {
                String id = n.getViewIdResourceName();
                if (id == null || !idExact.equals(id)) return false;
            }
            if (classExact != null) {
                CharSequence c = n.getClassName();
                if (c == null) return false;
                String s = c.toString();
                // Allow simple-name match: `EditText` matches
                // `android.widget.EditText`. Exact-equals path keeps the
                // unambiguous `class=android.widget.EditText` form working.
                if (!classExact.equals(s) && !s.endsWith("." + classExact)) return false;
            }
            if (pkgExact != null) {
                CharSequence p = n.getPackageName();
                if (p == null || !pkgExact.contentEquals(p)) return false;
            }
            return true;
        }

        @Override public String toString() {
            StringBuilder sb = new StringBuilder();
            if (textExact != null) sb.append(" text=").append(textExact);
            if (textSub != null)   sb.append(" text~=").append(textSub);
            if (descExact != null) sb.append(" desc=").append(descExact);
            if (descSub != null)   sb.append(" desc~=").append(descSub);
            if (idExact != null)   sb.append(" id=").append(idExact);
            if (classExact != null)sb.append(" class=").append(classExact);
            if (pkgExact != null)  sb.append(" pkg=").append(pkgExact);
            return sb.length() > 0 ? sb.substring(1) : "";
        }
    }

    /** Whitespace-split that keeps "quoted strings" together. */
    private static List<String> tokenize(String s) {
        List<String> out = new ArrayList<>();
        StringBuilder cur = new StringBuilder();
        boolean inQuote = false;
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '"') {
                inQuote = !inQuote;
                cur.append(c);
            } else if (Character.isWhitespace(c) && !inQuote) {
                if (cur.length() > 0) { out.add(cur.toString()); cur.setLength(0); }
            } else {
                cur.append(c);
            }
        }
        if (cur.length() > 0) out.add(cur.toString());
        return out;
    }

    private static String unquote(String v) {
        if (v.length() >= 2 && v.charAt(0) == '"' && v.charAt(v.length() - 1) == '"') {
            return v.substring(1, v.length() - 1);
        }
        return v;
    }

    private static byte[] ok(String s) { return s.getBytes(StandardCharsets.UTF_8); }
    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
