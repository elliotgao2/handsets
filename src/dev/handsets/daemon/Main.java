package dev.handsets.daemon;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.UiAutomation;
import android.os.HandlerThread;
import android.os.Looper;

import java.lang.reflect.Constructor;
import java.lang.reflect.Method;

public final class Main {

    public static void main(String[] args) {
        int port = 9008;
        for (String a : args) {
            if (a.startsWith("--port=")) {
                port = Integer.parseInt(a.substring("--port=".length()));
            }
        }

        try {
            disableHiddenApiRestrictions();
            if (Looper.getMainLooper() == null) {
                Looper.prepareMainLooper();
            }

            HandlerThread ht = new HandlerThread("hs-ua");
            ht.start();
            Looper looper = ht.getLooper();

            Class<?> connCls = Class.forName("android.app.UiAutomationConnection");
            Constructor<?> connCtor = connCls.getDeclaredConstructor();
            connCtor.setAccessible(true);
            Object conn = connCtor.newInstance();

            Class<?> iConnCls = Class.forName("android.app.IUiAutomationConnection");

            UiAutomation ua = createUiAutomation(looper, conn, iConnCls);
            connectWithRetry(ua);
            registerShutdownHook(ua);
            enableInteractiveWindows(ua);
            try {
                ua.setRunAsMonkey(true);
            } catch (Throwable t) {
                System.err.println("warn: setRunAsMonkey failed: " + t);
            }

            android.content.Context sysCtx = Binders.systemContext();
            Server.Handlers h = new Server.Handlers();
            h.dumper = new Dumper(ua);
            h.shot = new Screenshot(ua);
            h.input = new Input(ua);
            h.files = new Files();
            h.installer = new Installer(sysCtx);
            h.pm = new Pm(sysCtx);
            h.manifest = new Manifest(sysCtx);
            h.providers = new Providers(sysCtx);
            h.location = new Location();
            h.notifs = new Notifications();
            h.clip = new Clipboard();
            h.am = new Am(sysCtx);
            try { h.props = new Props(); }
            catch (Throwable t) { System.err.println("warn: Props init failed: " + t); }
            h.dumpsys = new Dumpsys();
            h.logcat = new Logcat();
            h.settings = new SettingsApi(sysCtx);
            h.shellExec = new ShellExec();
            h.wm = new Wm((android.hardware.display.DisplayManager)
                    sysCtx.getSystemService(android.content.Context.DISPLAY_SERVICE));
            h.lifecycle = new Lifecycle();
            h.uiEvents = new UiEvents(ua);
            h.nodes = new NodeActions(ua);
            h.state = new State(sysCtx, ua, h.uiEvents);
            Server server = new Server(h, port);

            // Warm the dump path in the background. First dump_active on a
            // cold JVM costs ~1 s (class loading + JIT of Dumper / Traverse
            // / JsonOut). Doing it on a daemon thread means `hs use`
            // returns fast and the dump path is hot by the time the user
            // (or an LLM loop) types `hs ui`. Dumper.dumpActive is
            // synchronized, so if the user's first call races, it serialises
            // cleanly on the same monitor — never worse than no warmup.
            final Server.Handlers handlersForWarmup = h;
            Thread warmup = new Thread(new Runnable() {
                @Override public void run() {
                    try {
                        long t0 = System.nanoTime();
                        handlersForWarmup.dumper.dumpActive();
                        long ms = (System.nanoTime() - t0) / 1_000_000;
                        System.err.println("hsd warmup: dump_active in " + ms + "ms");
                    } catch (Throwable t) {
                        System.err.println("warn: dump_active warmup failed: " + t);
                    }
                }
            }, "hsd-warmup");
            warmup.setDaemon(true);
            warmup.start();

            System.out.println("hsd ready (sdk=" + android.os.Build.VERSION.SDK_INT + ")");
            server.serve();
        } catch (Throwable t) {
            t.printStackTrace();
            System.exit(2);
        }
    }

    private static UiAutomation createUiAutomation(Looper looper, Object conn, Class<?> iConnCls)
            throws Exception {
        Constructor<UiAutomation> ctor;
        try {
            ctor = UiAutomation.class.getDeclaredConstructor(Looper.class, iConnCls);
        } catch (NoSuchMethodException e) {
            // Newer SDKs may only expose (Context, IUiAutomationConnection); fall back.
            try {
                Class<?> contextCls = Class.forName("android.content.Context");
                Constructor<UiAutomation> ctorCtx =
                        UiAutomation.class.getDeclaredConstructor(contextCls, iConnCls);
                ctorCtx.setAccessible(true);
                Object ctx = getSystemContext();
                return ctorCtx.newInstance(ctx, conn);
            } catch (Throwable t) {
                throw new IllegalStateException(
                        "no usable UiAutomation constructor on SDK "
                                + android.os.Build.VERSION.SDK_INT, t);
            }
        }
        ctor.setAccessible(true);
        return ctor.newInstance(looper, conn);
    }

    private static void enableInteractiveWindows(UiAutomation ua) {
        try {
            AccessibilityServiceInfo info = ua.getServiceInfo();
            if (info == null) return;
            info.flags |= AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS;
            ua.setServiceInfo(info);
        } catch (Throwable t) {
            System.err.println("warn: enableInteractiveWindows failed: " + t);
        }
    }

    private static void connectWithRetry(UiAutomation ua) throws Exception {
        Throwable last = null;
        for (int attempt = 0; attempt < 20; attempt++) {
            try {
                invokeConnect(ua);
                return;
            } catch (java.lang.reflect.InvocationTargetException ite) {
                last = ite.getCause() != null ? ite.getCause() : ite;
                String msg = last.toString();
                if (msg.contains("already registered")) {
                    Thread.sleep(250);
                    continue;
                }
                throw ite;
            }
        }
        throw new IllegalStateException("connect failed after retries", last);
    }

    private static void invokeConnect(UiAutomation ua) throws Exception {
        Method connect;
        try {
            connect = UiAutomation.class.getDeclaredMethod("connect", int.class);
            connect.setAccessible(true);
            connect.invoke(ua, 1);
            return;
        } catch (NoSuchMethodException ignored) {
        }
        connect = UiAutomation.class.getDeclaredMethod("connect");
        connect.setAccessible(true);
        connect.invoke(ua);
    }

    private static void registerShutdownHook(final UiAutomation ua) {
        Runtime.getRuntime().addShutdownHook(new Thread("hs-shutdown") {
            @Override public void run() {
                try {
                    Method disconnect = UiAutomation.class.getDeclaredMethod("disconnect");
                    disconnect.setAccessible(true);
                    disconnect.invoke(ua);
                } catch (Throwable ignored) {}
            }
        });
    }

    private static Object getSystemContext() throws Exception {
        Class<?> at = Class.forName("android.app.ActivityThread");
        Method systemMain = at.getMethod("systemMain");
        Object thread = systemMain.invoke(null);
        Method getSystemContext = at.getMethod("getSystemContext");
        return getSystemContext.invoke(thread);
    }

    private static void disableHiddenApiRestrictions() {
        try {
            Class<?> vmRuntime = Class.forName("dalvik.system.VMRuntime");
            Method getRuntime = vmRuntime.getDeclaredMethod("getRuntime");
            Object runtime = getRuntime.invoke(null);
            Method setExemptions = vmRuntime.getDeclaredMethod(
                    "setHiddenApiExemptions", String[].class);
            setExemptions.invoke(runtime, (Object) new String[]{"L"});
        } catch (Throwable t) {
            System.err.println("warn: could not lift hidden-api restrictions: " + t);
        }
    }

    private Main() {}
}
