package dev.handsets.daemon;

import android.content.ComponentName;
import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.res.AssetManager;
import android.content.res.XmlResourceParser;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * Walks an installed APK's binary AndroidManifest.xml to enumerate every
 * declared deeplink URI template — intent-filters with action.VIEW and
 * one or more {@code <data>} children. We go straight to the manifest
 * rather than {@link PackageManager#queryIntentActivities} because the
 * binder surface drops attributes (pathPattern, port, mimeType) we
 * need to reconstruct the URI shape the app declared.
 */
final class Manifest {

    private static final String ANDROID =
            "http://schemas.android.com/apk/res/android";

    private final Context ctx;

    Manifest(Context ctx) { this.ctx = ctx; }

    /** Emit one row per (intent-filter, &lt;data&gt;) pair with action.VIEW. */
    byte[] deeplinks(String pkg) {
        if (pkg == null || pkg.isEmpty()) return err("deeplinks-needs-pkg");

        String apkPath;
        try {
            PackageInfo pi = ctx.getPackageManager().getPackageInfo(pkg, 0);
            ApplicationInfo ai = pi.applicationInfo;
            apkPath = ai == null ? null : ai.sourceDir;
        } catch (Throwable t) {
            return err("pkg-not-found:" + pkg);
        }
        if (apkPath == null) return err("apk-path-missing:" + pkg);

        XmlResourceParser parser;
        try {
            // AssetManager() ctor and addAssetPath are @hide; reach them
            // through reflection. openXmlResourceParser then returns a
            // parser positioned at the start of the binary XML stream.
            AssetManager am = AssetManager.class.getConstructor().newInstance();
            AssetManager.class.getMethod("addAssetPath", String.class)
                    .invoke(am, apkPath);
            parser = (XmlResourceParser) AssetManager.class
                    .getMethod("openXmlResourceParser", int.class, String.class)
                    .invoke(am, 0, "AndroidManifest.xml");
        } catch (Throwable t) {
            return err("manifest-open-failed:" + t.getMessage());
        }

        StringBuilder out = new StringBuilder(2048);
        try {
            walk(parser, pkg, out);
        } catch (Throwable t) {
            return err("manifest-parse-failed:" + t.getMessage());
        } finally {
            parser.close();
        }

        if (out.length() == 0) {
            return ("no deeplinks declared in " + pkg)
                    .getBytes(StandardCharsets.UTF_8);
        }
        return out.toString().getBytes(StandardCharsets.UTF_8);
    }

    private static void walk(XmlResourceParser p, String pkg, StringBuilder out)
            throws Exception {
        String pkgAttr = pkg;
        String currentComp = null;
        boolean inFilter = false;
        boolean filterHasView = false;
        List<String> cats = new ArrayList<>();
        List<String> dataLines = new ArrayList<>();

        int ev;
        while ((ev = p.next()) != XmlResourceParser.END_DOCUMENT) {
            if (ev == XmlResourceParser.START_TAG) {
                String name = p.getName();
                if ("manifest".equals(name)) {
                    String pa = p.getAttributeValue(null, "package");
                    if (pa != null && !pa.isEmpty()) pkgAttr = pa;
                } else if ("activity".equals(name) || "activity-alias".equals(name)) {
                    currentComp = resolveComponent(pkgAttr,
                            p.getAttributeValue(ANDROID, "name"));
                } else if ("intent-filter".equals(name) && currentComp != null) {
                    inFilter = true;
                    filterHasView = false;
                    cats.clear();
                    dataLines.clear();
                } else if (inFilter && "action".equals(name)) {
                    if ("android.intent.action.VIEW".equals(
                            p.getAttributeValue(ANDROID, "name"))) {
                        filterHasView = true;
                    }
                } else if (inFilter && "category".equals(name)) {
                    String c = p.getAttributeValue(ANDROID, "name");
                    if (c != null) {
                        if (c.startsWith("android.intent.category.")) {
                            c = c.substring("android.intent.category.".length());
                        }
                        cats.add(c);
                    }
                } else if (inFilter && "data".equals(name)) {
                    String line = describeData(p);
                    if (!line.isEmpty()) dataLines.add(line);
                }
            } else if (ev == XmlResourceParser.END_TAG) {
                String name = p.getName();
                if ("intent-filter".equals(name) && inFilter) {
                    if (filterHasView && !dataLines.isEmpty()) {
                        String catStr = cats.isEmpty() ? "-" : String.join(",", cats);
                        for (String d : dataLines) {
                            out.append(pad(d, 60)).append("  ")
                               .append(pad(catStr, 22)).append("  ")
                               .append(currentComp).append('\n');
                        }
                    }
                    inFilter = false;
                } else if ("activity".equals(name) || "activity-alias".equals(name)) {
                    currentComp = null;
                }
            }
        }
    }

    /** Build a `scheme://host[:port][path]` template from one &lt;data&gt; tag. */
    private static String describeData(XmlResourceParser p) {
        String scheme  = p.getAttributeValue(ANDROID, "scheme");
        String host    = p.getAttributeValue(ANDROID, "host");
        String port    = p.getAttributeValue(ANDROID, "port");
        String path    = p.getAttributeValue(ANDROID, "path");
        String prefix  = p.getAttributeValue(ANDROID, "pathPrefix");
        String pattern = p.getAttributeValue(ANDROID, "pathPattern");
        String mime    = p.getAttributeValue(ANDROID, "mimeType");

        StringBuilder sb = new StringBuilder();
        if (scheme != null) sb.append(scheme).append("://");
        if (host   != null) sb.append(host);
        if (port   != null) sb.append(':').append(port);
        if      (path    != null) sb.append(path);
        else if (prefix  != null) sb.append(prefix).append('*');
        else if (pattern != null) sb.append(pattern);
        if (mime != null) {
            if (sb.length() > 0) sb.append("  ");
            sb.append("mime=").append(mime);
        }
        return sb.toString();
    }

    /** Resolve a possibly-relative activity name into pkg/.Class form. */
    private static String resolveComponent(String pkg, String activityName) {
        if (activityName == null || activityName.isEmpty()) return pkg + "/?";
        String full;
        if (activityName.startsWith(".")) full = pkg + activityName;
        else if (!activityName.contains(".")) full = pkg + "." + activityName;
        else full = activityName;
        return new ComponentName(pkg, full).flattenToShortString();
    }

    private static String pad(String s, int w) {
        if (s.length() >= w) return s;
        StringBuilder b = new StringBuilder(s);
        while (b.length() < w) b.append(' ');
        return b.toString();
    }

    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
