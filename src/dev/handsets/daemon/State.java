package dev.handsets.daemon;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.ActivityManager;
import android.app.UiAutomation;
import android.content.ComponentName;
import android.content.Context;
import android.content.res.Configuration;
import android.hardware.display.DisplayManager;
import android.net.ConnectivityManager;
import android.net.LinkAddress;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.wifi.WifiInfo;
import android.net.wifi.WifiManager;
import android.os.BatteryManager;
import android.os.Handler;
import android.os.HandlerThread;
import android.view.Display;
import android.view.accessibility.AccessibilityEvent;

import java.net.Inet4Address;
import java.net.InetAddress;

import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Fast state-query surface. Every method here is a single direct-binder call
 * (or a manager wrapper that's itself a thin shim over one) — the goal is
 * sub-millisecond wire time so callers don't have to parse {@code dumpsys}
 * output.
 *
 * Each public method returns a UTF-8 byte payload ready to ship as one frame.
 */
final class State {

    private final Context ctx;
    private final BatteryManager bm;
    private final ActivityManager am;
    private final UiAutomation ua;            // for accessibility events
    private final DisplayManager displayMgr;  // for screen state changes

    // Cached binders/methods so the hot path doesn't pay reflective lookup
    // each call. All set lazily on first use; null means "not available on
    // this SDK level / not yet probed".
    private volatile Object iPowerManager;
    private volatile Method  isInteractive1;   // (int displayId)
    private volatile Method  isInteractive0;   // ()

    private volatile Object iActivityTaskManager;
    private volatile Method  getTasks;

    private volatile Object iActivityManager;
    private volatile Method  getRunningAppProcesses;

    // ---------- device-JSON cache ----------
    //
    // device() collects ~5 binder fields. At ~1ms each the wire-level cost
    // dominates. We refresh in the background — but event-driven instead of
    // on a fixed timer:
    //   • UiAutomation accessibility events flag changes to the top activity.
    //   • DisplayManager.DisplayListener flags changes to the screen state.
    //   • A 5s heartbeat covers fields we don't have an event hook for
    //     (battery level/charging — those move slowly enough that 5s is fine).
    // Bursts get coalesced by a small post-trigger debounce.
    private static final long HEARTBEAT_MS = 5_000L;
    private static final long DEBOUNCE_MS  = 50L;
    private final AtomicReference<byte[]> cachedDevice = new AtomicReference<>();
    private final AtomicReference<long[]> cachedDeviceStamp = new AtomicReference<>(new long[]{0L});
    private final Object dirtyLock = new Object();
    private volatile boolean dirty = true;
    private volatile Thread refresher;
    private volatile boolean listenersRegistered;

    /** Push-mode subscribers. Invoked on every fresh snapshot. */
    public interface SnapshotListener {
        void onSnapshot(byte[] json);
    }
    private final CopyOnWriteArrayList<SnapshotListener> subs = new CopyOnWriteArrayList<>();

    public void subscribe(SnapshotListener l) {
        subs.add(l);
        // Push the current snapshot synchronously so the subscriber doesn't
        // have to wait for the next change to populate its mirror.
        byte[] snap = cachedDevice.get();
        if (snap == null) snap = device();
        try { l.onSnapshot(snap); } catch (Throwable ignored) {}
        ensureRefresher();
    }

    public void unsubscribe(SnapshotListener l) {
        subs.remove(l);
    }

    private final UiEvents uiEvents;

    State(Context ctx, UiAutomation ua, UiEvents uiEvents) {
        this.ctx = ctx;
        this.ua = ua;
        this.uiEvents = uiEvents;
        this.bm = (BatteryManager) ctx.getSystemService(Context.BATTERY_SERVICE);
        this.am = (ActivityManager) ctx.getSystemService(Context.ACTIVITY_SERVICE);
        this.displayMgr = (DisplayManager) ctx.getSystemService(Context.DISPLAY_SERVICE);
    }

    // ---------- power ----------

    byte[] interactive() {
        try {
            Object pm = powerBinder();
            if (isInteractive1 != null) {
                Boolean b = (Boolean) isInteractive1.invoke(pm, Display.DEFAULT_DISPLAY);
                return bytes(Boolean.toString(b));
            }
            if (isInteractive0 != null) {
                Boolean b = (Boolean) isInteractive0.invoke(pm);
                return bytes(Boolean.toString(b));
            }
            return err("isInteractive-not-found");
        } catch (Throwable t) {
            return err("interactive-failed:" + t.getMessage());
        }
    }

    // ---------- battery ----------

    byte[] batteryLevel() {
        try {
            int level = bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY);
            return bytes(Integer.toString(level));
        } catch (Throwable t) {
            return err("battery-level-failed:" + t.getMessage());
        }
    }

    byte[] batteryCharging() {
        try {
            return bytes(Boolean.toString(bm.isCharging()));
        } catch (Throwable t) {
            return err("battery-charging-failed:" + t.getMessage());
        }
    }

    // ---------- foreground / tasks ----------

    byte[] topActivity() {
        try {
            ComponentName cn = resolveTopComponent();
            return bytes(cn == null ? "null" : cn.flattenToShortString());
        } catch (Throwable t) {
            return err("top-failed:" + t.getMessage());
        }
    }

    private ComponentName resolveTopComponent() throws Exception {
        Object iatm = activityTaskBinder();
        if (iatm == null) return null;
        Method gt = getTasks;
        if (gt == null) return null;
        Class<?>[] p = gt.getParameterTypes();
        Object[] args = new Object[p.length];
        // First arg is maxNum: 1. Remaining bool/int slots: false/0.
        for (int i = 0; i < p.length; i++) {
            if (i == 0 && p[i] == int.class) args[i] = 1;
            else if (p[i] == int.class) args[i] = 0;
            else if (p[i] == boolean.class) args[i] = false;
            else args[i] = null;
        }
        @SuppressWarnings("unchecked")
        List<ActivityManager.RunningTaskInfo> tasks =
                (List<ActivityManager.RunningTaskInfo>) gt.invoke(iatm, args);
        if (tasks == null || tasks.isEmpty()) return null;
        return tasks.get(0).topActivity;
    }

    // ---------- running processes ----------

    byte[] procs() {
        try {
            Object iam = activityBinder();
            if (iam == null || getRunningAppProcesses == null) return err("not-bound");
            @SuppressWarnings("unchecked")
            List<ActivityManager.RunningAppProcessInfo> ps =
                    (List<ActivityManager.RunningAppProcessInfo>) getRunningAppProcesses.invoke(iam);
            StringBuilder sb = new StringBuilder(2048);
            if (ps != null) {
                for (ActivityManager.RunningAppProcessInfo i : ps) {
                    sb.append(i.pid).append('\t')
                      .append(i.uid).append('\t')
                      .append(i.importance).append('\t')
                      .append(i.processName).append('\n');
                }
            }
            return sb.toString().getBytes(StandardCharsets.UTF_8);
        } catch (Throwable t) {
            return err("procs-failed:" + t.getMessage());
        }
    }

    // ---------- composite snapshot ----------

    /**
     * Single-frame JSON with every common state field a UI driver wants.
     * Returns a cached snapshot that's at most {@value #CACHE_REFRESH_MS}ms
     * stale. First call starts the background refresher; subsequent calls
     * just hand back the latest byte[] (no binder hops).
     */
    byte[] device() {
        ensureRefresher();
        byte[] snap = cachedDevice.get();
        if (snap != null) return snap;
        // First call before the refresher has produced anything — block once.
        snap = computeDevice();
        cachedDevice.set(snap);
        cachedDeviceStamp.set(new long[]{System.currentTimeMillis()});
        return snap;
    }

    /** Force-collect a fresh snapshot, update the cache, return the bytes. */
    byte[] deviceFresh() {
        byte[] snap = computeDevice();
        cachedDevice.set(snap);
        cachedDeviceStamp.set(new long[]{System.currentTimeMillis()});
        ensureRefresher();
        return snap;
    }

    private synchronized void ensureRefresher() {
        if (refresher != null && refresher.isAlive()) return;
        registerListeners();
        Thread t = new Thread(new Runnable() {
            @Override public void run() {
                while (!Thread.currentThread().isInterrupted()) {
                    // Block until something flags dirty, or HEARTBEAT_MS goes by.
                    synchronized (dirtyLock) {
                        while (!dirty) {
                            try { dirtyLock.wait(HEARTBEAT_MS); }
                            catch (InterruptedException ie) { return; }
                            // Heartbeat wake: also treat as dirty so battery
                            // level / charging stay reasonably current.
                            break;
                        }
                        dirty = false;
                    }
                    // Brief debounce to coalesce bursts (e.g. one app launch
                    // generates multiple TYPE_WINDOW_* events in <50ms).
                    try { Thread.sleep(DEBOUNCE_MS); }
                    catch (InterruptedException ie) { return; }
                    // Drain anything that came in during the debounce window.
                    synchronized (dirtyLock) { dirty = false; }
                    try {
                        byte[] s = computeDevice();
                        cachedDevice.set(s);
                        cachedDeviceStamp.set(new long[]{System.currentTimeMillis()});
                        // Fan-out to push-mode subscribers. COW list, so no
                        // lock; listeners must be non-blocking.
                        if (!subs.isEmpty()) {
                            for (SnapshotListener l : subs) {
                                try { l.onSnapshot(s); } catch (Throwable ignored2) {}
                            }
                        }
                    } catch (Throwable ignored) {}
                }
            }
        }, "hs-state-refresh");
        t.setDaemon(true);
        t.start();
        refresher = t;
    }

    /** Mark the snapshot stale; wakes the refresh thread. */
    private void markDirty() {
        synchronized (dirtyLock) {
            dirty = true;
            dirtyLock.notifyAll();
        }
    }

    private synchronized void registerListeners() {
        if (listenersRegistered) return;
        listenersRegistered = true;

        // 1. Accessibility events → top-activity change detection. Route
        //    through the shared UiEvents dispatcher so wait-for-* primitives
        //    can subscribe to the same stream.
        uiEvents.subscribe(new UiEvents.Consumer() {
            @Override public void onEvent(AccessibilityEvent ev) {
                int t = ev.getEventType();
                if (t == AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED
                        || t == AccessibilityEvent.TYPE_WINDOWS_CHANGED) {
                    markDirty();
                }
            }
        });

        // 1b. Package install/remove → invalidate app counts.
        try {
            android.content.IntentFilter pkgFilter = new android.content.IntentFilter();
            pkgFilter.addAction(android.content.Intent.ACTION_PACKAGE_ADDED);
            pkgFilter.addAction(android.content.Intent.ACTION_PACKAGE_REMOVED);
            pkgFilter.addAction(android.content.Intent.ACTION_PACKAGE_REPLACED);
            pkgFilter.addDataScheme("package");
            HandlerThread pkgT = new HandlerThread("hs-state-pkg");
            pkgT.setDaemon(true);
            pkgT.start();
            Handler pkgH = new Handler(pkgT.getLooper());
            android.content.BroadcastReceiver pkgR = new android.content.BroadcastReceiver() {
                @Override public void onReceive(Context c, android.content.Intent i) {
                    System.err.println("hs: pkg broadcast " + i.getAction()
                            + " data=" + i.getDataString());
                    invalidateAppCounts();
                    markDirty();
                }
            };
            // System broadcasts (PACKAGE_*) come from outside our process, so
            // the receiver must be EXPORTED on API 34+. The 5-arg overload
            // (flag) exists from API 33; use it via reflection so we keep
            // compiling against any SDK.
            try {
                java.lang.reflect.Method m = Context.class.getMethod(
                        "registerReceiver",
                        android.content.BroadcastReceiver.class,
                        android.content.IntentFilter.class,
                        String.class, Handler.class, int.class);
                m.invoke(ctx, pkgR, pkgFilter, null, pkgH, /* RECEIVER_EXPORTED */ 2);
            } catch (Throwable noFlagOverload) {
                ctx.registerReceiver(pkgR, pkgFilter, null, pkgH);
            }
        } catch (Throwable t) {
            System.err.println("warn: package broadcast listener failed: " + t);
        }

        // 2. Display state changes → screen on/off.
        try {
            HandlerThread ht = new HandlerThread("hs-state-display");
            ht.setDaemon(true);
            ht.start();
            Handler handler = new Handler(ht.getLooper());
            displayMgr.registerDisplayListener(new DisplayManager.DisplayListener() {
                @Override public void onDisplayAdded(int id)   { markDirty(); }
                @Override public void onDisplayRemoved(int id) { markDirty(); }
                @Override public void onDisplayChanged(int id) { markDirty(); }
            }, handler);
        } catch (Throwable t) {
            System.err.println("warn: State display listener failed: " + t);
        }

        // 3. Configuration changes → uiMode (dark/light theme), locale,
        //    orientation. ACTION_CONFIGURATION_CHANGED is what UiModeManager
        //    fires when night mode toggles; ComponentCallbacks dispatch isn't
        //    reliable from a shell-uid Context (our Resources don't get
        //    updated by ActivityThread the way an app process's would), so
        //    we listen for the system broadcast directly.
        try {
            android.content.IntentFilter cfgFilter = new android.content.IntentFilter(
                    android.content.Intent.ACTION_CONFIGURATION_CHANGED);
            HandlerThread cfgT = new HandlerThread("hs-state-cfg");
            cfgT.setDaemon(true);
            cfgT.start();
            Handler cfgH = new Handler(cfgT.getLooper());
            android.content.BroadcastReceiver cfgR = new android.content.BroadcastReceiver() {
                @Override public void onReceive(Context c, android.content.Intent i) {
                    markDirty();
                }
            };
            try {
                java.lang.reflect.Method m = Context.class.getMethod(
                        "registerReceiver",
                        android.content.BroadcastReceiver.class,
                        android.content.IntentFilter.class,
                        String.class, Handler.class, int.class);
                m.invoke(ctx, cfgR, cfgFilter, null, cfgH, /* RECEIVER_EXPORTED */ 2);
            } catch (Throwable noFlagOverload) {
                ctx.registerReceiver(cfgR, cfgFilter, null, cfgH);
            }
        } catch (Throwable t) {
            System.err.println("warn: State config-change listener failed: " + t);
        }
    }

    /** Static fields — props, total RAM/storage, CPU model. Computed once. */
    private volatile String cachedStaticJson;

    private synchronized String staticJson() {
        if (cachedStaticJson != null) return cachedStaticJson;
        StringBuilder sb = new StringBuilder(512);
        sb.append("\"model\":")        .append(jsonStr(getprop("ro.product.model"))).append(',');
        sb.append("\"manufacturer\":") .append(jsonStr(getprop("ro.product.manufacturer"))).append(',');
        sb.append("\"device\":")       .append(jsonStr(getprop("ro.product.device"))).append(',');
        sb.append("\"release\":")      .append(jsonStr(getprop("ro.build.version.release"))).append(',');
        sb.append("\"codename\":")     .append(jsonStr(getprop("ro.build.version.codename"))).append(',');
        sb.append("\"build_type\":")   .append(jsonStr(getprop("ro.build.type"))).append(',');
        sb.append("\"fingerprint\":")  .append(jsonStr(getprop("ro.build.fingerprint"))).append(',');
        sb.append("\"abi\":")          .append(jsonStr(getprop("ro.product.cpu.abi"))).append(',');
        sb.append("\"abi_list\":")     .append(jsonStr(getprop("ro.product.cpu.abilist"))).append(',');
        sb.append("\"bootloader\":")   .append(jsonStr(getprop("ro.bootloader"))).append(',');
        sb.append("\"hardware\":")     .append(jsonStr(getprop("ro.hardware"))).append(',');
        sb.append("\"locale\":")       .append(jsonStr(getprop("persist.sys.locale"))).append(',');
        sb.append("\"timezone\":")     .append(jsonStr(getprop("persist.sys.timezone"))).append(',');
        sb.append("\"kernel\":")       .append(jsonStr(readKernelVersion())).append(',');
        sb.append("\"cpu_cores\":")    .append(readCpuCores()).append(',');
        sb.append("\"cpu_model\":")    .append(jsonStr(readCpuModel())).append(',');
        sb.append("\"total_ram_kb\":") .append(readMemTotalKb()).append(',');
        sb.append("\"total_storage_kb\":").append(readStorageTotalKb()).append(',');
        sb.append("\"sdk\":")          .append(android.os.Build.VERSION.SDK_INT).append(',');
        cachedStaticJson = sb.toString();
        return cachedStaticJson;
    }

    // ---------- app-count cache ----------
    //
    // Ideally event-driven: PACKAGE_ADDED / PACKAGE_REMOVED / PACKAGE_REPLACED
    // BroadcastReceiver below invalidates this cache the moment a package
    // changes. In practice the receiver doesn't fire from our app_process
    // Context on every Android build (same calling-identity wall as the
    // SettingsProvider), so we *also* TTL the cache as a safety net.
    // Slightly under the heartbeat so every heartbeat-driven snapshot
    // refresh re-counts and picks up any silent install / uninstall.
    private static final long APP_COUNT_TTL_MS = HEARTBEAT_MS - 500;
    private volatile long[] appCounts;          // [total, third-party]
    private volatile long   appCountsAt;        // ms-since-epoch when computed

    private long[] appCounts() {
        long[] cur = appCounts;
        if (cur != null && System.currentTimeMillis() - appCountsAt < APP_COUNT_TTL_MS) {
            return cur;
        }
        synchronized (this) {
            long[] now = appCounts;
            if (now != null && System.currentTimeMillis() - appCountsAt < APP_COUNT_TTL_MS) {
                return now;
            }
            long total = 0, third = 0;
            try {
                for (android.content.pm.PackageInfo p :
                        ctx.getPackageManager().getInstalledPackages(0)) {
                    total++;
                    android.content.pm.ApplicationInfo ai = p.applicationInfo;
                    if (ai != null && (ai.flags & android.content.pm.ApplicationInfo.FLAG_SYSTEM) == 0) {
                        third++;
                    }
                }
            } catch (Throwable ignored) {}
            appCounts = new long[]{total, third};
            appCountsAt = System.currentTimeMillis();
            return appCounts;
        }
    }

    private void invalidateAppCounts() { appCounts = null; }

    private byte[] computeDevice() {
        StringBuilder sb = new StringBuilder(1024);
        sb.append('{');

        // Static (one-time): model / sdk / abi / kernel / totals / locale.
        sb.append(staticJson());

        // Dynamic — live binder fields.
        appendField(sb, "interactive", interactive(), true);
        appendField(sb, "battery_level", batteryLevel(), true);
        appendField(sb, "battery_charging", batteryCharging(), true);
        sb.append("\"battery_temp_c\":").append(readBatteryTempC()).append(',');
        try {
            ComponentName top = resolveTopComponent();
            sb.append("\"top_activity\":")
              .append(top == null ? "null" : ("\"" + top.flattenToShortString() + "\""))
              .append(',');
        } catch (Throwable ignored) {
            sb.append("\"top_activity\":null,");
        }

        // Dynamic — derived from /proc and StatFs (cheap, ~ms total).
        sb.append("\"uptime_s\":")        .append(readUptimeS()).append(',');
        sb.append("\"mem_available_kb\":") .append(readMemAvailableKb()).append(',');
        sb.append("\"storage_free_kb\":")  .append(readStorageFreeKb()).append(',');

        // Dynamic — app counts (event-driven via PACKAGE_* broadcasts).
        long[] ac = appCounts();
        sb.append("\"app_count\":")       .append(ac[0]).append(',');
        sb.append("\"app_count_3rd\":")   .append(ac[1]).append(',');

        // Dynamic — theme (changes when user toggles dark mode).
        sb.append("\"theme\":")           .append(jsonStr(readTheme())).append(',');

        // Dynamic — network state (transport + IP).
        sb.append("\"network\":")         .append(jsonStr(readNetworkType())).append(',');
        sb.append("\"ip\":")              .append(jsonStr(readIp())).append(',');
        sb.append("\"wifi_ssid\":")       .append(jsonStr(readWifiSsid())).append(',');

        // Dynamic — display.
        Display d = ctx.getSystemService(android.hardware.display.DisplayManager.class)
                .getDisplay(Display.DEFAULT_DISPLAY);
        android.graphics.Point sz = new android.graphics.Point();
        d.getRealSize(sz);
        sb.append("\"width\":").append(sz.x).append(',');
        sb.append("\"height\":").append(sz.y).append(',');
        sb.append("\"rotation\":").append(d.getRotation()).append(',');

        sb.append("\"computed_at_ms\":").append(System.currentTimeMillis());
        sb.append('}');
        return sb.toString().getBytes(StandardCharsets.UTF_8);
    }

    // ---------- field readers ----------

    private static volatile Method spGet;
    private static String getprop(String key) {
        try {
            if (spGet == null) {
                spGet = Class.forName("android.os.SystemProperties")
                        .getDeclaredMethod("get", String.class);
            }
            Object v = spGet.invoke(null, key);
            return v == null ? "" : v.toString();
        } catch (Throwable t) { return ""; }
    }

    private static String readFile(String path) {
        try (java.io.BufferedReader r = new java.io.BufferedReader(new java.io.FileReader(path))) {
            StringBuilder sb = new StringBuilder(256);
            String line;
            while ((line = r.readLine()) != null) sb.append(line).append('\n');
            return sb.toString();
        } catch (Throwable t) { return ""; }
    }

    private static String readKernelVersion() {
        String raw = readFile("/proc/version");
        if (raw.isEmpty()) return "";
        // "Linux version 6.6.30-android15-…" → grab the version token.
        String[] parts = raw.split("\\s+");
        for (int i = 0; i + 1 < parts.length; i++) {
            if ("version".equals(parts[i])) return parts[i + 1];
        }
        return raw.trim();
    }

    private static long readCpuCores() {
        String raw = readFile("/proc/cpuinfo");
        long n = 0;
        for (String line : raw.split("\n")) if (line.startsWith("processor")) n++;
        return n;
    }

    private static String readCpuModel() {
        String raw = readFile("/proc/cpuinfo");
        for (String line : raw.split("\n")) {
            if (line.startsWith("Hardware")) return line.substring(line.indexOf(':') + 1).trim();
            if (line.startsWith("model name")) return line.substring(line.indexOf(':') + 1).trim();
        }
        return "";
    }

    private static long parseKBField(String content, String prefix) {
        for (String line : content.split("\n")) {
            if (line.startsWith(prefix)) {
                String[] toks = line.trim().split("\\s+");
                if (toks.length >= 2) {
                    try { return Long.parseLong(toks[1]); } catch (NumberFormatException ignored) {}
                }
            }
        }
        return 0L;
    }

    private static long readMemTotalKb() {
        return parseKBField(readFile("/proc/meminfo"), "MemTotal:");
    }

    private static long readMemAvailableKb() {
        return parseKBField(readFile("/proc/meminfo"), "MemAvailable:");
    }

    private static long readUptimeS() {
        String raw = readFile("/proc/uptime").trim();
        if (raw.isEmpty()) return 0;
        try { return (long) Double.parseDouble(raw.split("\\s+")[0]); }
        catch (Throwable t) { return 0; }
    }

    private static long readStorageTotalKb() {
        try {
            android.os.StatFs sf = new android.os.StatFs("/data");
            return sf.getTotalBytes() / 1024;
        } catch (Throwable t) { return 0; }
    }

    private static long readStorageFreeKb() {
        try {
            android.os.StatFs sf = new android.os.StatFs("/data");
            return sf.getAvailableBytes() / 1024;
        } catch (Throwable t) { return 0; }
    }

    /** Battery temperature in °C (driver value is tenths-of-a-degree). */
    private static String readBatteryTempC() {
        String raw = readFile("/sys/class/power_supply/battery/temp").trim();
        if (raw.isEmpty()) return "null";
        try {
            int tenths = Integer.parseInt(raw);
            return Double.toString(tenths / 10.0);
        } catch (NumberFormatException nf) { return "null"; }
    }

    private String readTheme() {
        // The shell-UID Context we run under has no app Resources attached,
        // so its Configuration stays frozen at UI_MODE_NIGHT_UNDEFINED no
        // matter what the user toggles, and UiModeManager.getNightMode() in
        // this process can also lag behind the system service. The
        // authoritative source is the Settings.Secure entry the
        // UiModeManagerService itself writes to: 1=NO, 2=YES, 0/3=AUTO/CUSTOM.
        try {
            int v = android.provider.Settings.Secure.getInt(
                    ctx.getContentResolver(), "ui_night_mode", -1);
            if (v == 2) return "dark";
            if (v == 1) return "light";
        } catch (Throwable ignored) {}
        try {
            android.app.UiModeManager uim = ctx.getSystemService(android.app.UiModeManager.class);
            if (uim != null) {
                int mode = uim.getNightMode();
                if (mode == android.app.UiModeManager.MODE_NIGHT_YES) return "dark";
                if (mode == android.app.UiModeManager.MODE_NIGHT_NO)  return "light";
            }
        } catch (Throwable ignored) {}
        try {
            int night = ctx.getResources().getConfiguration().uiMode
                    & Configuration.UI_MODE_NIGHT_MASK;
            return (night == Configuration.UI_MODE_NIGHT_YES) ? "dark" : "light";
        } catch (Throwable t) { return ""; }
    }

    /** "wifi" / "cellular" / "ethernet" / "vpn" / "" if no usable link.
     *  Prefers ConnectivityManager but falls back to interface-name
     *  heuristics — the emulator and some odd shell-UID contexts return
     *  a null active network from ConnectivityManager. */
    private String readNetworkType() {
        try {
            ConnectivityManager cm = ctx.getSystemService(ConnectivityManager.class);
            Network active = cm.getActiveNetwork();
            if (active != null) {
                NetworkCapabilities caps = cm.getNetworkCapabilities(active);
                if (caps != null) {
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI))     return "wifi";
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) return "cellular";
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) return "ethernet";
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN))      return "vpn";
                    return "other";
                }
            }
        } catch (Throwable ignored) {}
        // Fallback: pick the first non-loopback interface that has an IPv4.
        try {
            java.util.Enumeration<java.net.NetworkInterface> ifs =
                    java.net.NetworkInterface.getNetworkInterfaces();
            while (ifs.hasMoreElements()) {
                java.net.NetworkInterface ni = ifs.nextElement();
                if (!ni.isUp() || ni.isLoopback()) continue;
                if (!has4(ni)) continue;
                String n = ni.getName();
                if (n.startsWith("wlan"))  return "wifi";
                if (n.startsWith("eth"))   return "ethernet";
                if (n.startsWith("rmnet")) return "cellular";
                if (n.startsWith("tun") || n.startsWith("ppp")) return "vpn";
                return n;
            }
        } catch (Throwable ignored) {}
        return "";
    }

    private static boolean has4(java.net.NetworkInterface ni) {
        java.util.Enumeration<InetAddress> a = ni.getInetAddresses();
        while (a.hasMoreElements()) {
            if (a.nextElement() instanceof Inet4Address) return true;
        }
        return false;
    }

    /** First non-loopback, non-link-local IPv4 address on any up interface. */
    private String readIp() {
        // ConnectivityManager first (cleaner attribution).
        try {
            ConnectivityManager cm = ctx.getSystemService(ConnectivityManager.class);
            Network active = cm.getActiveNetwork();
            if (active != null) {
                LinkProperties lp = cm.getLinkProperties(active);
                if (lp != null) {
                    for (LinkAddress la : lp.getLinkAddresses()) {
                        InetAddress a = la.getAddress();
                        if (a instanceof Inet4Address && !a.isLoopbackAddress() && !a.isLinkLocalAddress()) {
                            return a.getHostAddress();
                        }
                    }
                }
            }
        } catch (Throwable ignored) {}
        // Fallback: walk NetworkInterface — works on the emulator where
        // ConnectivityManager returns no active network even though
        // eth0/wlan0 has a real IPv4 address.
        try {
            java.util.Enumeration<java.net.NetworkInterface> ifs =
                    java.net.NetworkInterface.getNetworkInterfaces();
            while (ifs.hasMoreElements()) {
                java.net.NetworkInterface ni = ifs.nextElement();
                if (!ni.isUp() || ni.isLoopback()) continue;
                java.util.Enumeration<InetAddress> a = ni.getInetAddresses();
                while (a.hasMoreElements()) {
                    InetAddress addr = a.nextElement();
                    if (addr instanceof Inet4Address && !addr.isLinkLocalAddress()) {
                        return addr.getHostAddress();
                    }
                }
            }
        } catch (Throwable ignored) {}
        return "";
    }

    /** Connected wifi SSID, stripped of WifiInfo's surrounding quotes. */
    private String readWifiSsid() {
        try {
            WifiManager wm = ctx.getSystemService(WifiManager.class);
            if (wm == null) return "";
            @SuppressWarnings("deprecation")
            WifiInfo info = wm.getConnectionInfo();
            if (info == null) return "";
            String s = info.getSSID();
            if (s == null || s.equals("<unknown ssid>")) return "";
            if (s.length() >= 2 && s.charAt(0) == '"' && s.charAt(s.length() - 1) == '"') {
                s = s.substring(1, s.length() - 1);
            }
            return s;
        } catch (Throwable t) { return ""; }
    }

    private static String jsonStr(String s) {
        if (s == null) return "null";
        return "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"") + "\"";
    }

    private static void appendField(StringBuilder sb, String key, byte[] valBytes, boolean trailingComma) {
        String v = new String(valBytes, StandardCharsets.UTF_8);
        boolean quote = !"true".equals(v) && !"false".equals(v) && !v.matches("-?\\d+");
        if (v.startsWith("ERR:")) { v = "null"; quote = false; }
        sb.append('"').append(key).append("\":");
        if (quote) sb.append('"').append(v.replace("\"", "\\\"")).append('"');
        else sb.append(v);
        if (trailingComma) sb.append(',');
    }

    // ---------- lazy binder/method caches ----------

    private Object powerBinder() {
        Object p = iPowerManager;
        if (p != null) return p;
        synchronized (this) {
            if (iPowerManager != null) return iPowerManager;
            Object svc = Binders.asInterface(
                    "android.os.IPowerManager$Stub", Binders.service("power"));
            // Probe both overloads exactly once.
            for (Method m : svc.getClass().getMethods()) {
                if (!"isInteractive".equals(m.getName())) continue;
                Class<?>[] pt = m.getParameterTypes();
                if (pt.length == 1 && pt[0] == int.class && isInteractive1 == null) {
                    isInteractive1 = m;
                } else if (pt.length == 0 && isInteractive0 == null) {
                    isInteractive0 = m;
                }
            }
            iPowerManager = svc;
            return svc;
        }
    }

    private Object activityTaskBinder() {
        Object p = iActivityTaskManager;
        if (p != null) return p;
        synchronized (this) {
            if (iActivityTaskManager != null) return iActivityTaskManager;
            Object svc = Binders.iActivityTaskManagerOrNull();
            if (svc == null) return null;
            // Pick the widest `getTasks` overload — first arg is max task count.
            Method best = null;
            for (Method m : svc.getClass().getMethods()) {
                if (!"getTasks".equals(m.getName())) continue;
                Class<?>[] pt = m.getParameterTypes();
                if (pt.length >= 1 && pt[0] == int.class
                        && (best == null || pt.length > best.getParameterTypes().length)) {
                    best = m;
                }
            }
            getTasks = best;
            iActivityTaskManager = svc;
            return svc;
        }
    }

    private Object activityBinder() {
        Object p = iActivityManager;
        if (p != null) return p;
        synchronized (this) {
            if (iActivityManager != null) return iActivityManager;
            Object svc = Binders.iActivityManager();
            for (Method m : svc.getClass().getMethods()) {
                if ("getRunningAppProcesses".equals(m.getName())
                        && m.getParameterTypes().length == 0) {
                    getRunningAppProcesses = m;
                    break;
                }
            }
            iActivityManager = svc;
            return svc;
        }
    }

    private static byte[] bytes(String s) { return s.getBytes(StandardCharsets.UTF_8); }
    private static byte[] err(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }
}
