package dev.handsets.daemon;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Read-only view of the LocationManager's last-known fixes.
 *
 * Strategy: spawn {@code dumpsys location} and scan for the
 * {@code Location[...]} substring that android.location.Location#toString
 * emits. That toString format is stable across AOSP versions:
 *   Location[<provider> <lat>,<lon>[ hAcc=N][ et=…][ alt=N][ vel=N][ bear=N] … mock?]
 *
 * Reaching ILocationManager.getLastLocation directly would skip the
 * dumpsys process spawn, but the AIDL signature changes meaningfully
 * across API levels (LocationRequest, attribution tag, LastLocationRequest)
 * and dumpsys already contains exactly the data we want. One ~30 ms shell
 * invocation per call is fine for this verb.
 *
 * Output is NDJSON — same shape as {@link Providers}: first line is the
 * column-name array, then one row per provider.
 */
final class Location {

    // Captures provider name, lat, lon, and the rest-of-bracket tail
    // (key=value attributes + optional " mock" sentinel).
    private static final Pattern LOC = Pattern.compile(
            "Location\\[\\s*([A-Za-z_][A-Za-z_0-9]*)\\s+"
            + "(-?\\d+(?:\\.\\d+)?),(-?\\d+(?:\\.\\d+)?)"
            + "([^\\]]*)\\]");

    private static final Pattern KV = Pattern.compile(
            "([A-Za-z]+)=(-?\\d+(?:\\.\\d+)?)");

    byte[] last() {
        Process p;
        try {
            p = new ProcessBuilder("/system/bin/dumpsys", "location")
                    .redirectErrorStream(true).start();
        } catch (Exception e) {
            return err("dumpsys-spawn:" + e.getMessage());
        }

        // Preserve first-seen order; the freshest "last location" block
        // tends to appear before per-provider history.
        Map<String, double[]> fixes = new LinkedHashMap<>();
        Map<String, Boolean> mocks = new LinkedHashMap<>();
        try (BufferedReader br = new BufferedReader(
                new InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))) {
            String ln;
            while ((ln = br.readLine()) != null) {
                Matcher m = LOC.matcher(ln);
                while (m.find()) {
                    String prov = m.group(1);
                    if (fixes.containsKey(prov)) continue;
                    double lat = Double.parseDouble(m.group(2));
                    double lon = Double.parseDouble(m.group(3));
                    String tail = m.group(4);
                    fixes.put(prov, new double[] {
                            lat, lon,
                            kv(tail, "hAcc"),
                            kv(tail, "alt"),
                            kv(tail, "vel"),
                            kv(tail, "bear"),
                    });
                    mocks.put(prov, tail.contains(" mock"));
                }
            }
        } catch (Exception e) {
            return err("dumpsys-read:" + e.getMessage());
        } finally {
            try { p.destroy(); } catch (Throwable ignored) {}
        }

        StringBuilder out = new StringBuilder(256);
        out.append("[\"provider\",\"lat\",\"lon\",\"accuracy_m\",")
           .append("\"altitude_m\",\"speed_mps\",\"bearing_deg\",\"mock\"]\n");
        for (Map.Entry<String, double[]> e : fixes.entrySet()) {
            double[] v = e.getValue();
            out.append('[').append('"').append(e.getKey()).append("\",");
            out.append(v[0]).append(',').append(v[1]).append(',');
            appendNum(out, v[2]); out.append(',');
            appendNum(out, v[3]); out.append(',');
            appendNum(out, v[4]); out.append(',');
            appendNum(out, v[5]); out.append(',');
            out.append(mocks.get(e.getKey()) ? "true" : "false");
            out.append("]\n");
        }
        return out.toString().getBytes(StandardCharsets.UTF_8);
    }

    private static double kv(String tail, String key) {
        Matcher m = KV.matcher(tail);
        while (m.find()) {
            if (m.group(1).equals(key)) {
                try { return Double.parseDouble(m.group(2)); }
                catch (NumberFormatException ignored) { return Double.NaN; }
            }
        }
        return Double.NaN;
    }

    private static void appendNum(StringBuilder out, double v) {
        if (Double.isNaN(v)) out.append("null");
        else                 out.append(v);
    }

    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
