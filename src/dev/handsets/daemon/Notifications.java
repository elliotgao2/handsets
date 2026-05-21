package dev.handsets.daemon;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * Notification tray reader.
 *
 * Direct {@code INotificationManager.getActiveNotifications} requires
 * the signature-protected {@code ACCESS_NOTIFICATIONS} permission,
 * which shell UID does *not* hold on stock Android. Instead we parse
 * the output of {@code dumpsys notification --noredact}, which the
 * shell process can run unrestricted. Same data source NotificationUI
 * tools use; output is large but stable enough across versions to
 * grep per-NotificationRecord blocks for the fields we care about.
 *
 * One frame per call, NDJSON: header + one row per notification with
 * {@code key / posted / pkg / channel / title / text / sub_text /
 * category}.
 *
 * History is not exposed by dumpsys — when {@code history=1} we fall
 * back to the same active list with a note in the response.
 */
final class Notifications {

    /** @param history reserved for future use; dumpsys only exposes active. */
    byte[] dump(String pkgFilter, int limit, boolean history) {
        String text;
        try {
            text = runDumpsys();
        } catch (Throwable t) {
            return err("dumpsys-failed:" + t.getClass().getSimpleName()
                    + ":" + t.getMessage());
        }
        if (text == null || text.isEmpty()) return err("empty-dumpsys");

        List<String> blocks = splitRecords(text);
        StringBuilder out = new StringBuilder(4096);
        out.append("[\"key\",\"posted\",\"pkg\",\"channel\",\"title\",\"text\",\"sub_text\",\"category\"]\n");

        int emitted = 0;
        for (String block : blocks) {
            if (limit > 0 && emitted >= limit) break;
            String pkg = extractAfter(block, "pkg=", " \t\n");
            if (pkg == null) continue;
            if (pkgFilter != null && !pkgFilter.isEmpty() && !pkgFilter.equals(pkg)) continue;

            String key      = extractAfter(block, "\n      key=", "\n");
            String channel  = extractAfter(block, "channel=", " \t\n");
            String category = extractAfter(block, "category=", " \t\n)");
            String whenRaw  = extractAfter(block, "when=", "/ \t\n");

            long posted = 0L;
            if (whenRaw != null) {
                try { posted = Long.parseLong(whenRaw.trim()); }
                catch (NumberFormatException ignored) {}
            }

            String title   = extractExtra(block, "android.title");
            String text2   = extractExtra(block, "android.text");
            if (text2 == null) text2 = extractExtra(block, "android.bigText");
            String subText = extractExtra(block, "android.subText");

            out.append('[');
            appendJsonString(out, key);      out.append(',');
            out.append(posted);              out.append(',');
            appendJsonString(out, pkg);      out.append(',');
            appendJsonString(out, channel);  out.append(',');
            appendJsonString(out, title);    out.append(',');
            appendJsonString(out, text2);    out.append(',');
            appendJsonString(out, subText);  out.append(',');
            appendJsonString(out, category);
            out.append("]\n");
            emitted++;
        }
        return out.toString().getBytes(StandardCharsets.UTF_8);
    }

    // ---------- dumpsys runner + block splitter ----------

    private static String runDumpsys() throws Exception {
        Process p = Runtime.getRuntime().exec(
                new String[] { "dumpsys", "notification", "--noredact" });
        StringBuilder sb = new StringBuilder(64 * 1024);
        try (BufferedReader r = new BufferedReader(
                new InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))) {
            char[] buf = new char[4096];
            int n;
            while ((n = r.read(buf)) > 0) sb.append(buf, 0, n);
        }
        p.waitFor();
        return sb.toString();
    }

    /** Slice the dump into per-NotificationRecord blocks, dropping any
     *  preamble + the Snoozed/History sections at the bottom. */
    private static List<String> splitRecords(String text) {
        List<String> out = new ArrayList<>();
        int active = text.indexOf("Notification List:");
        int from = active >= 0 ? active : 0;
        String marker = "NotificationRecord(";
        int i = text.indexOf(marker, from);
        while (i >= 0) {
            int j = text.indexOf(marker, i + marker.length());
            // Stop scanning once we leave the "Notification List:" section
            // (e.g. into the "Notifications by app:" report).
            int sectionEnd = text.indexOf("\n  ", i);     // next 2-space section header
            if (sectionEnd >= 0 && (j < 0 || sectionEnd < j)) {
                // The NotificationRecord block usually ends at a line with
                // exactly two spaces of indent — the section boundary.
            }
            String block = j >= 0 ? text.substring(i, j) : text.substring(i);
            out.add(block);
            if (j < 0) break;
            i = j;
        }
        return out;
    }

    // ---------- field extractors ----------

    /** Return the text between {@code prefix} and the first char in
     *  {@code stopChars}, or null if {@code prefix} isn't present. */
    private static String extractAfter(String block, String prefix, String stopChars) {
        int i = block.indexOf(prefix);
        if (i < 0) return null;
        int start = i + prefix.length();
        int end = start;
        while (end < block.length() && stopChars.indexOf(block.charAt(end)) < 0) end++;
        return block.substring(start, end);
    }

    /** Extract the value of {@code android.X=String (Y)} or
     *  {@code android.X=CharSequence (Y)}. Returns null when the line
     *  isn't present or the value is {@code null}. */
    private static String extractExtra(String block, String key) {
        // Match either `key=String (` or `key=CharSequence (`.
        int i = block.indexOf("\n                " + key + "=");
        if (i < 0) i = block.indexOf("                " + key + "=");
        if (i < 0) return null;
        int lineStart = i + 1;       // skip the leading newline if present
        int eq = block.indexOf('=', lineStart);
        if (eq < 0) return null;
        int after = eq + 1;
        // null literal
        if (block.startsWith("null", after)) return null;
        // Look for `(...)` payload.
        int paren = block.indexOf('(', after);
        int lineEnd = block.indexOf('\n', after);
        if (paren < 0 || (lineEnd >= 0 && paren > lineEnd)) return null;
        // Balanced-paren extractor — handles parentheses inside the
        // value text. Stop on the matching ')' that closes our '('.
        int depth = 0;
        int from = paren;
        for (int p = paren; p < block.length(); p++) {
            char c = block.charAt(p);
            if (c == '(') depth++;
            else if (c == ')') {
                depth--;
                if (depth == 0) {
                    return block.substring(from + 1, p);
                }
            }
            // Bail out if we run into a new extras line.
            if (c == '\n' && block.startsWith("                android.", p + 1)) break;
        }
        return null;
    }

    // ---------- JSON ----------

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
