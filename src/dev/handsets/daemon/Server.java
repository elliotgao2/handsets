package dev.handsets.daemon;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicBoolean;

public final class Server {

    private static final int MAX_CMD = 256;

    /** Bag of handler classes wired up in {@link Main}; lets the constructor
     *  stay legible as we grow the command surface. */
    public static final class Handlers {
        public Dumper dumper;
        public Screenshot shot;
        public Input input;
        public Files files;
        public Installer installer;
        public Pm pm;
        public Manifest manifest;
        public Providers providers;
        public Location location;
        public Notifications notifs;
        public Clipboard clip;
        public Am am;
        public Props props;
        public Dumpsys dumpsys;
        public Logcat logcat;
        public SettingsApi settings;
        public ShellExec shellExec;
        public Wm wm;
        public Lifecycle lifecycle;
        public State state;
        public UiEvents uiEvents;
        public NodeActions nodes;
    }

    private final Handlers h;
    private final int port;
    private final AtomicBoolean running = new AtomicBoolean(true);

    public Server(Handlers h, int port) {
        this.h = h;
        this.port = port;
    }

    public void serve() throws IOException {
        ServerSocket ss = new ServerSocket(port, 8, InetAddress.getByName("127.0.0.1"));
        ss.setReuseAddress(true);
        System.out.println("hsd listening on 127.0.0.1:" + port);
        while (running.get()) {
            final Socket s = ss.accept();
            Thread t = new Thread(new Runnable() {
                @Override public void run() { handle(s); }
            }, "hs-conn");
            t.setDaemon(true);
            t.start();
        }
        ss.close();
    }

    private void handle(Socket s) {
        try {
            s.setTcpNoDelay(true);
            DataInputStream in = new DataInputStream(s.getInputStream());
            DataOutputStream out = new DataOutputStream(s.getOutputStream());
            while (!s.isClosed()) {
                int len;
                try {
                    len = in.readInt();
                } catch (IOException eof) {
                    break;
                }
                if (len <= 0 || len > MAX_CMD) {
                    writeFrame(out, errBytes("BAD_ARG:bad-length:" + len));
                    break;
                }
                byte[] buf = new byte[len];
                in.readFully(buf);
                // UTF-8 (was US_ASCII): keeps backward-compat for the
                // all-ASCII verb syntax and lets `clip_set <CJK>` /
                // `text <CJK>` / `node_set_text value="<CJK>"` round-trip
                // properly.
                String cmd = new String(buf, StandardCharsets.UTF_8).trim();

                byte[] resp;
                String head = cmd;
                int sp = cmd.indexOf(' ');
                if (sp >= 0) head = cmd.substring(0, sp);
                switch (head) {
                    case "ping":
                        resp = "pong".getBytes(StandardCharsets.UTF_8);
                        break;
                    case "dump":
                        resp = utf8(safeDumpAll());
                        break;
                    case "dump_active":
                        resp = utf8(safeDumpActive());
                        break;
                    case "info":
                        resp = utf8(h.shot.sourceSize().x + " " + h.shot.sourceSize().y);
                        break;
                    case "screenshot":
                        resp = safeScreenshot(cmd);
                        break;
                    case "stream":
                        runStream(cmd, out);
                        return;     // socket is single-purpose once streaming
                    case "stream_h264":
                        runH264Stream(cmd, out);
                        return;
                    case "stream_tilejpeg":
                        runTileStream(cmd, out);
                        return;
                    case "keyframe":
                        H264Streamer.requestKeyframeAll();
                        resp = utf8("ok n=" + H264Streamer.activeCount());
                        break;
                    case "tap":
                    case "swipe":
                    case "down":
                    case "move":
                    case "up":
                    case "scroll":
                    case "key":
                    case "text":
                        resp = runInput(head, cmd);
                        break;
                    case "pull":
                        runPull(cmd, out);
                        continue;     // pull writes its own frames + terminator
                    case "push":
                        resp = runPush(cmd, in);
                        break;
                    case "install":
                        resp = runInstall(cmd, in);
                        break;
                    case "pm_list":
                        resp = runPmList(cmd);
                        break;
                    case "pm_path":
                        resp = runPmPath(cmd);
                        break;
                    case "pm_uninstall":
                        resp = runPmUninstall(cmd);
                        break;
                    case "pm_grant":
                    case "pm_revoke":
                        resp = runPmPerm(head, cmd);
                        break;
                    case "am_start":
                        resp = runAmStart(cmd);
                        break;
                    case "am_force_stop":
                        resp = runAmForceStop(cmd);
                        break;
                    case "am_kill":
                        resp = runAmKill(cmd);
                        break;
                    case "am_broadcast":
                        resp = runAmBroadcast(cmd);
                        break;
                    case "swipe_dir":
                        resp = runSwipeDir(cmd);
                        break;
                    case "getprop":
                        resp = runGetProp(cmd);
                        break;
                    case "setprop":
                        resp = runSetProp(cmd);
                        break;
                    case "dumpsys":
                        runDumpsys(cmd, out);
                        continue;     // streams its own frames + terminator
                    case "logcat":
                        runLogcat(cmd, out);
                        continue;
                    case "settings_get":
                        resp = runSettingsGet(cmd);
                        break;
                    case "settings_put":
                        resp = runSettingsPut(cmd);
                        break;
                    case "shell":
                        runShell(cmd, out);
                        continue;
                    case "wm_info":
                        resp = h.wm.info();
                        break;
                    case "wm_rotation":
                        resp = runWmRotation(cmd);
                        break;
                    case "install_multi":
                        resp = runInstallMulti(cmd, in);
                        break;
                    case "monitor":
                        runMonitor(out);
                        continue;
                    case "state":
                        resp = runState(cmd);
                        break;
                    case "state_watch":
                        runStateWatch(out);
                        continue;
                    case "wait_for_idle":
                        resp = runWaitForIdle(cmd);
                        break;
                    case "wait_for_text":
                        resp = runWaitForText(cmd);
                        break;
                    case "wait_for_activity":
                        resp = runWaitForActivity(cmd);
                        break;
                    case "tap_and_dump":
                        resp = runTapAndDump(cmd);
                        break;
                    case "tap_and_settle":
                        resp = runTapAndSettle(cmd);
                        break;
                    case "node_click":
                        resp = runNodeClick(cmd, false);
                        break;
                    case "node_long_click":
                        resp = runNodeClick(cmd, true);
                        break;
                    case "node_set_text":
                        resp = runNodeSetText(cmd);
                        break;
                    case "node_scroll":
                        resp = runNodeScroll(cmd);
                        break;
                    case "node_focus":
                        resp = runNodeFocus(cmd);
                        break;
                    case "submit":
                        resp = runSubmit(cmd);
                        break;
                    case "paste":
                        resp = runPaste(cmd);
                        break;
                    case "deeplinks":
                        resp = runDeeplinks(cmd);
                        break;
                    case "sms":
                        resp = runSms(cmd);
                        break;
                    case "calls":
                        resp = runCalls(cmd);
                        break;
                    case "contacts":
                        resp = runContacts(cmd);
                        break;
                    case "calendar":
                        resp = runCalendar(cmd);
                        break;
                    case "location":
                        resp = h.location.last();
                        break;
                    case "notifications":
                        resp = runNotifications(cmd);
                        break;
                    case "clip_get":
                        resp = h.clip.get();
                        break;
                    case "clip_set":
                        resp = h.clip.set(afterHead(cmd));
                        break;
                    case "clip_watch":
                        runClipWatch(cmd, out);
                        continue;
                    case "quit":
                        writeFrame(out, "bye".getBytes(StandardCharsets.UTF_8));
                        running.set(false);
                        System.exit(0);
                        return;
                    default:
                        resp = errBytes("UNKNOWN_CMD:" + head);
                }
                writeFrame(out, resp);
            }
        } catch (Throwable t) {
            t.printStackTrace();
        } finally {
            try { s.close(); } catch (IOException ignored) {}
        }
    }

    private String safeDumpAll() {
        try {
            return h.dumper.dumpAll();
        } catch (Throwable t) {
            return "ERR:INTERNAL:dump-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage();
        }
    }

    private String safeDumpActive() {
        try {
            return h.dumper.dumpActive();
        } catch (Throwable t) {
            return "ERR:INTERNAL:dump-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage();
        }
    }

    private byte[] safeScreenshot(String cmd) {
        try {
            Screenshot.CaptureArgs args = parseScreenshotArgs(cmd);
            return h.shot.capture(args);
        } catch (Throwable t) {
            return errBytes("screenshot-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    private byte[] runInput(String head, String cmd) {
        try {
            // `text` is special-cased so spaces inside the typed string survive.
            if ("text".equals(head)) {
                String body = cmd.length() > 5 ? cmd.substring(5) : "";
                h.input.text(body);
                return "ok".getBytes(StandardCharsets.UTF_8);
            }
            InputArgs a = parseInputArgs(cmd);
            switch (head) {
                case "tap": {
                    // Reject coordinates outside the display. Off-screen taps
                    // dispatch into nothing but the kernel input layer happily
                    // accepts them and we'd return "ok" — the worst failure
                    // mode for a UI driver. Callers that match an a11y node
                    // whose post-scroll bounds went stale should see an
                    // explicit OFF_SCREEN and retry/refresh.
                    android.graphics.Point sz = h.shot.sourceSize();
                    if (sz.x > 0 && sz.y > 0
                            && (a.x < 0 || a.x >= sz.x || a.y < 0 || a.y >= sz.y)) {
                        return errBytes("OFF_SCREEN:x=" + a.x + " y=" + a.y
                                + " display=" + sz.x + "x" + sz.y);
                    }
                    h.input.tap(a.x, a.y);
                    break;
                }
                case "swipe":
                    h.input.swipe(a.x1, a.y1, a.x2, a.y2, a.dur > 0 ? a.dur : 300);
                    break;
                case "down":
                    h.input.pointerDown(a.x, a.y);
                    break;
                case "move":
                    h.input.pointerMove(a.x, a.y);
                    break;
                case "up":
                    h.input.pointerUp(a.x, a.y);
                    break;
                case "scroll":
                    h.input.scroll(a.x, a.y, a.dy);
                    break;
                case "key":
                    if (a.code != 0) {
                        h.input.key(a.code);
                    } else if (a.keyName != null) {
                        h.input.keyByName(a.keyName);
                    } else {
                        return errBytes("key-needs-name-or-code");
                    }
                    break;
                default:
                    return errBytes("unknown-input:" + head);
            }
            return "ok".getBytes(StandardCharsets.UTF_8);
        } catch (Throwable t) {
            return errBytes(head + "-failed:" + t.getClass().getSimpleName()
                    + ":" + t.getMessage());
        }
    }

    private static final class InputArgs {
        int x, y, x1, y1, x2, y2, dur, dy, code;
        String keyName;
    }

    /** Parse k=v tokens plus a single positional name (for `key BACK`). */
    private static InputArgs parseInputArgs(String cmd) {
        InputArgs a = new InputArgs();
        int sp = cmd.indexOf(' ');
        if (sp < 0) return a;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return a;
        for (String tok : tail.split("\\s+")) {
            int eq = tok.indexOf('=');
            if (eq < 0) {
                // positional — treat as key name for `key BACK` style
                if (a.keyName == null) a.keyName = tok;
                continue;
            }
            String k = tok.substring(0, eq);
            String v = tok.substring(eq + 1);
            try {
                switch (k) {
                    case "x":    a.x  = Integer.parseInt(v); break;
                    case "y":    a.y  = Integer.parseInt(v); break;
                    case "x1":   a.x1 = Integer.parseInt(v); break;
                    case "y1":   a.y1 = Integer.parseInt(v); break;
                    case "x2":   a.x2 = Integer.parseInt(v); break;
                    case "y2":   a.y2 = Integer.parseInt(v); break;
                    case "dur":  a.dur = Integer.parseInt(v); break;
                    case "dy":   a.dy  = Integer.parseInt(v); break;
                    case "code": a.code = Integer.parseInt(v); break;
                    default:
                        // ignore unknown for forward-compat
                }
            } catch (NumberFormatException ignored) {}
        }
        return a;
    }

    private void runTileStream(String cmd, DataOutputStream out) {
        try {
            Screenshot.CaptureArgs args = parseScreenshotArgs(cmd);
            if (args.longEdge == 0 && !args.max) args.max = true;  // default native
            Screenshot.Mirror mirror = h.shot.mirrorFor(args);
            int tile = args.tile > 0 ? args.tile : 128;
            int q = clampQuality(args.quality);
            TileStreamer s = new TileStreamer(mirror, q, tile, out);
            s.serve();
        } catch (Throwable t) {
            try {
                writeFrame(out, errBytes("tilejpeg-failed:" + t.getClass().getSimpleName()
                        + ":" + t.getMessage()));
            } catch (IOException ignored) {}
        }
    }

    private void runH264Stream(String cmd, DataOutputStream out) {
        try {
            Screenshot.CaptureArgs args = parseScreenshotArgs(cmd);
            // H.264 stream defaults to native resolution like JPEG stream does.
            if (args.longEdge == 0 && !args.max) args.max = true;
            int srcLong = Math.max(h.shot.sourceSize().x, h.shot.sourceSize().y);
            int targetLongEdge = args.max
                    ? srcLong
                    : (args.longEdge > 0 ? Math.min(args.longEdge, srcLong) : 768);
            H264Streamer s = new H264Streamer(
                    h.shot.displayManager(),
                    h.shot.sourceSize(),
                    targetLongEdge,
                    args.bitrateKbps,
                    args.fps,
                    args.gopSec,
                    out);
            s.serve();
        } catch (Throwable t) {
            try {
                writeFrame(out, errBytes("h264-failed:" + t.getClass().getSimpleName()
                        + ":" + t.getMessage()));
            } catch (IOException ignored) {}
        }
    }

    private void runStream(String cmd, DataOutputStream out) {
        try {
            Screenshot.CaptureArgs args = parseScreenshotArgs(cmd);
            // Streams default to native resolution to match the snapshot users liked.
            if (args.longEdge == 0 && !args.max) args.max = true;
            Screenshot.Mirror mirror = h.shot.mirrorFor(args);
            Streamer s = new Streamer(mirror, clampQuality(args.quality), args.fps, out);
            s.serve();
        } catch (Throwable t) {
            try {
                writeFrame(out, errBytes("stream-failed:" + t.getClass().getSimpleName()
                        + ":" + t.getMessage()));
            } catch (IOException ignored) {}
        }
    }

    private static int clampQuality(int q) {
        if (q <= 0) return 80;
        if (q > 100) return 100;
        return q;
    }

    private static Screenshot.CaptureArgs parseScreenshotArgs(String cmd) {
        Screenshot.CaptureArgs a = new Screenshot.CaptureArgs();
        // cmd = "screenshot[ k=v[ k=v...]]"
        int sp = cmd.indexOf(' ');
        if (sp < 0) return a;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return a;
        for (String tok : tail.split("\\s+")) {
            int eq = tok.indexOf('=');
            if (eq <= 0) continue;
            String k = tok.substring(0, eq);
            String v = tok.substring(eq + 1);
            try {
                switch (k) {
                    case "size":
                        a.longEdge = Integer.parseInt(v);
                        break;
                    case "q":
                    case "quality":
                        a.quality = Integer.parseInt(v);
                        break;
                    case "fmt":
                    case "format":
                        a.format = v;
                        break;
                    case "max":
                        a.max = !"0".equals(v) && !"false".equalsIgnoreCase(v);
                        break;
                    case "fps":
                        a.fps = Integer.parseInt(v);
                        break;
                    case "bitrate":
                    case "kbps":
                        a.bitrateKbps = Integer.parseInt(v);
                        break;
                    case "tile":
                        a.tile = Integer.parseInt(v);
                        break;
                    case "gop":
                        a.gopSec = Integer.parseInt(v);
                        break;
                    case "secure_check":
                    case "secure":
                        a.secureCheck = !"0".equals(v) && !"false".equalsIgnoreCase(v);
                        break;
                    default:
                        // unknown key — ignore so old clients keep working
                }
            } catch (NumberFormatException ignored) {}
        }
        return a;
    }

    private static byte[] utf8(String s) {
        return s.getBytes(StandardCharsets.UTF_8);
    }

    private static byte[] errBytes(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }

    private static void writeFrame(DataOutputStream out, byte[] bytes) throws IOException {
        out.writeInt(bytes.length);
        out.write(bytes);
        out.flush();
    }

    // ---------- adb-like command handlers ----------

    private void runPull(String cmd, DataOutputStream out) throws IOException {
        FileArgs a = parseFileArgs(cmd);
        if (a.path == null) {
            writeFrame(out, errBytes("pull-needs-path"));
            out.writeInt(0);
            out.flush();
            return;
        }
        try {
            h.files.pull(a.path, out);
        } catch (Throwable t) {
            // Files.pull already best-effort-terminates the stream.
            System.err.println("pull failed: " + t);
        }
    }

    private byte[] runPush(String cmd, DataInputStream in) {
        FileArgs a = parseFileArgs(cmd);
        if (a.path == null) return errBytes("push-needs-path");
        return h.files.push(a.path, a.mode, a.size, in);
    }

    private byte[] runInstall(String cmd, DataInputStream in) {
        FileArgs a = parseFileArgs(cmd);
        return h.installer.install(a.size, a.reinstall, a.grant, in);
    }

    private byte[] runPmList(String cmd) {
        boolean thirdParty = cmd.contains(" 3") || cmd.contains("third=1");
        boolean systemOnly = cmd.contains(" s") || cmd.contains("system=1");
        return h.pm.list(thirdParty, systemOnly);
    }

    private byte[] runPmPath(String cmd) {
        String pkg = positional(cmd);
        if (pkg == null) return errBytes("pm_path-needs-pkg");
        return h.pm.path(pkg);
    }

    private byte[] runPmUninstall(String cmd) {
        String pkg = positional(cmd);
        if (pkg == null) return errBytes("pm_uninstall-needs-pkg");
        return h.pm.uninstall(pkg);
    }

    private byte[] runPmPerm(String head, String cmd) {
        String[] parts = positionalPair(cmd);
        if (parts == null) return errBytes(head + "-needs-pkg-and-perm");
        if ("pm_grant".equals(head)) return h.pm.grant(parts[0], parts[1]);
        return h.pm.revoke(parts[0], parts[1]);
    }

    private byte[] runAmStart(String cmd) {
        AmArgs a = parseAmArgs(cmd);
        if (a.component == null) return errBytes("am_start-needs-n=pkg/.Class");
        return h.am.start(a.component, a.action, a.data, a.flags);
    }

    private byte[] runAmForceStop(String cmd) {
        String pkg = positional(cmd);
        if (pkg == null) return errBytes("am_force_stop-needs-pkg");
        return h.am.forceStop(pkg);
    }

    private byte[] runAmKill(String cmd) {
        String pkg = positional(cmd);
        if (pkg == null) return errBytes("am_kill-needs-pkg");
        return h.am.kill(pkg);
    }

    private byte[] runAmBroadcast(String cmd) {
        AmArgs a = parseAmArgs(cmd);
        return h.am.broadcast(a.action, a.component, a.data);
    }

    /**
     * `swipe_dir <left|right|up|down> [dur=N]` — synthesises an 80%-of-screen
     * swipe in the named direction starting from the screen centre. Screen
     * dimensions come from the existing Screenshot mirror so we don't
     * round-trip another binder call per swipe.
     */
    private byte[] runSwipeDir(String cmd) {
        String dir = positional(cmd);
        if (dir == null) return errBytes("swipe_dir-needs-direction");
        // 500 ms over 60% of the screen reads as a deliberate drag to
        // launchers + ListView/RecyclerView. Override with `dur=N` for flings.
        int dur = (int) longArg(cmd, "dur", 500);
        android.graphics.Point sz = h.shot.sourceSize();
        int w = sz.x, ht = sz.y;
        if (w <= 0 || ht <= 0) return errBytes("swipe_dir-no-display-size");
        int hi = 8, lo = 2;   // tenths of the screen: 0.8 → 0.2 = 60% travel
        // Horizontal swipes need a small Y drift, otherwise they stay
        // inside the row's bounds on vertical-scroll lists and Android's
        // default View.onTouchEvent fires onClick on ACTION_UP (the row's
        // pointInView check only cancels the click when the touch leaves
        // the view's bounds + touch-slop). 12% of screen-height (~370 px
        // on a 3120 px display) is several rows tall — enough to cancel.
        int drift = ht * 12 / 100;
        int x1, y1, x2, y2;
        switch (dir.toLowerCase()) {
            case "left":  x1 = w * hi / 10; y1 = ht / 2 - drift / 2; x2 = w * lo / 10; y2 = ht / 2 + drift / 2; break;
            case "right": x1 = w * lo / 10; y1 = ht / 2 + drift / 2; x2 = w * hi / 10; y2 = ht / 2 - drift / 2; break;
            case "up":    x1 = w / 2;       y1 = ht * hi / 10;       x2 = w / 2;       y2 = ht * lo / 10;       break;
            case "down":  x1 = w / 2;       y1 = ht * lo / 10;       x2 = w / 2;       y2 = ht * hi / 10;       break;
            default: return errBytes("swipe_dir-bad-direction:" + dir);
        }
        try {
            h.input.swipe(x1, y1, x2, y2, dur);
            return utf8("ok dir=" + dir.toLowerCase()
                    + " x1=" + x1 + " y1=" + y1
                    + " x2=" + x2 + " y2=" + y2
                    + " dur=" + dur);
        } catch (Throwable t) {
            return errBytes("swipe-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    // ---------- arg parsers for the new commands ----------

    private static final class FileArgs {
        String path;
        int mode;
        long size;
        boolean reinstall;
        boolean grant;
    }

    private static FileArgs parseFileArgs(String cmd) {
        FileArgs a = new FileArgs();
        int sp = cmd.indexOf(' ');
        if (sp < 0) return a;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return a;
        for (String tok : tail.split("\\s+")) {
            int eq = tok.indexOf('=');
            if (eq <= 0) continue;
            String k = tok.substring(0, eq);
            String v = tok.substring(eq + 1);
            try {
                switch (k) {
                    case "path":      a.path = v; break;
                    case "mode":      a.mode = Integer.parseInt(v, 8); break;
                    case "size":      a.size = Long.parseLong(v); break;
                    case "reinstall": a.reinstall = !"0".equals(v); break;
                    case "grant":     a.grant = !"0".equals(v); break;
                    default:
                        // ignore unknown for forward-compat
                }
            } catch (NumberFormatException ignored) {}
        }
        return a;
    }

    private static final class AmArgs {
        String component, action, data;
        int flags;
    }

    private static AmArgs parseAmArgs(String cmd) {
        AmArgs a = new AmArgs();
        int sp = cmd.indexOf(' ');
        if (sp < 0) return a;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return a;
        for (String tok : tail.split("\\s+")) {
            int eq = tok.indexOf('=');
            if (eq <= 0) continue;
            String k = tok.substring(0, eq);
            String v = tok.substring(eq + 1);
            try {
                switch (k) {
                    case "n":     a.component = v; break;
                    case "a":     a.action = v; break;
                    case "d":     a.data = v; break;
                    case "f":     a.flags = Integer.decode(v); break;
                    default:
                        // ignore
                }
            } catch (NumberFormatException ignored) {}
        }
        return a;
    }

    /** Return the first whitespace-separated token after the command head. */
    private static String positional(String cmd) {
        int sp = cmd.indexOf(' ');
        if (sp < 0) return null;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return null;
        int sp2 = tail.indexOf(' ');
        return sp2 < 0 ? tail : tail.substring(0, sp2);
    }

    // ---------- second-wave command handlers ----------

    private byte[] runGetProp(String cmd) {
        String key = positional(cmd);
        if (key == null) return errBytes("getprop-needs-key");
        return h.props.doGet(key);
    }

    private byte[] runSetProp(String cmd) {
        String[] pair = positionalPair(cmd);
        if (pair == null) return errBytes("setprop-needs-key-and-value");
        return h.props.doSet(pair[0], pair[1]);
    }

    private void runDumpsys(String cmd, DataOutputStream out) {
        String tail = afterHead(cmd);
        if (tail == null || tail.isEmpty()) {
            writeErrAndTerminator(out, "dumpsys-needs-service");
            return;
        }
        String[] parts = tail.split("\\s+");
        String svc = parts[0];
        String[] args = new String[parts.length - 1];
        System.arraycopy(parts, 1, args, 0, args.length);
        h.dumpsys.dump(svc, args, out);
    }

    private void runLogcat(String cmd, DataOutputStream out) {
        String tail = afterHead(cmd);
        java.util.List<String> args = new java.util.ArrayList<>();
        if (tail != null && !tail.isEmpty()) {
            for (String t : tail.split("\\s+")) args.add(t);
        }
        h.logcat.stream(args, out);
    }

    private byte[] runSettingsGet(String cmd) {
        String[] parts = positionalPair(cmd);
        if (parts == null) return errBytes("settings_get-needs-namespace-and-key");
        return h.settings.get(parts[0], parts[1]);
    }

    private byte[] runSettingsPut(String cmd) {
        // Need namespace + key + value (3 positional tokens).
        String tail = afterHead(cmd);
        if (tail == null) return errBytes("settings_put-needs-namespace-key-value");
        String[] parts = tail.split("\\s+", 3);
        if (parts.length < 3) return errBytes("settings_put-needs-namespace-key-value");
        return h.settings.put(parts[0], parts[1], parts[2]);
    }

    private void runShell(String cmd, DataOutputStream out) {
        String tail = afterHead(cmd);
        if (tail == null || tail.isEmpty()) {
            writeErrAndTerminator(out, "shell-needs-argv");
            return;
        }
        java.util.List<String> argv = new java.util.ArrayList<>();
        for (String t : tail.split("\\s+")) argv.add(t);
        h.shellExec.run(argv, out);
    }

    private byte[] runWmRotation(String cmd) {
        String v = positional(cmd);
        if (v == null) return errBytes("wm_rotation-needs-value");
        try { return h.wm.setRotation(Integer.parseInt(v)); }
        catch (NumberFormatException nf) { return errBytes("bad-rotation:" + v); }
    }

    private byte[] runInstallMulti(String cmd, DataInputStream in) {
        // install_multi sizes=N0,N1,N2 [reinstall=1] [grant=1]
        FileArgs base = parseFileArgs(cmd);
        long[] sizes = null;
        for (String tok : cmd.split("\\s+")) {
            if (tok.startsWith("sizes=")) {
                String[] parts = tok.substring("sizes=".length()).split(",");
                sizes = new long[parts.length];
                try {
                    for (int i = 0; i < parts.length; i++) sizes[i] = Long.parseLong(parts[i]);
                } catch (NumberFormatException e) {
                    return errBytes("install_multi-bad-sizes");
                }
            }
        }
        if (sizes == null || sizes.length == 0) {
            return errBytes("install_multi-needs-sizes");
        }
        return h.installer.installMulti(sizes, base.reinstall, base.grant, in);
    }

    private void runMonitor(DataOutputStream out) {
        h.lifecycle.monitor(out);
    }

    // ---------- wait-for / composite handlers ----------

    private static final long DEFAULT_IDLE_MS = 200;
    private static final long DEFAULT_TIMEOUT_MS = 5000;
    private static final long DEFAULT_TAP_TIMEOUT_MS = 2000;

    private byte[] runWaitForIdle(String cmd) {
        long idle    = longArg(cmd, "idle_ms", DEFAULT_IDLE_MS);
        long timeout = longArg(cmd, "timeout_ms", DEFAULT_TIMEOUT_MS);
        long elapsed = h.uiEvents.waits().awaitIdle(idle, timeout);
        if (elapsed < 0) return errBytes("TIMEOUT:wait_for_idle:idle_ms=" + idle + " timeout_ms=" + timeout);
        return utf8("ok elapsed=" + elapsed);
    }

    private byte[] runWaitForText(String cmd) {
        final String text = strArg(cmd, "text");
        if (text == null || text.isEmpty()) return errBytes("wait_for_text-needs-text");
        final String mode = strArgOr(cmd, "match", "sub");
        long timeout = longArg(cmd, "timeout_ms", DEFAULT_TIMEOUT_MS);
        final NodeActions.Selector sel = textSelector(text, mode);
        final android.view.accessibility.AccessibilityNodeInfo[] found = new android.view.accessibility.AccessibilityNodeInfo[1];
        long elapsed = h.uiEvents.waits().awaitPredicate(new WaitRegistry.Predicate() {
            @Override public boolean check() {
                found[0] = h.nodes.find(sel);
                return found[0] != null;
            }
        }, timeout);
        if (elapsed < 0) return errBytes("TIMEOUT:wait_for_text:" + text);
        android.graphics.Rect r = new android.graphics.Rect();
        found[0].getBoundsInScreen(r);
        return utf8("ok x=" + r.left + " y=" + r.top
                + " w=" + r.width() + " h=" + r.height()
                + " elapsed=" + elapsed);
    }

    private byte[] runWaitForActivity(String cmd) {
        final String component = strArg(cmd, "n");
        if (component == null) return errBytes("wait_for_activity-needs-n");
        long timeout = longArg(cmd, "timeout_ms", DEFAULT_TIMEOUT_MS);
        // Match policy:
        //   1. exact match against the live topActivity flatten string
        //   2. or package-prefix match: "com.foo/.X" matches any top
        //      activity starting with "com.foo/", since Android often
        //      resolves the alias the caller asked for to a different
        //      concrete component (e.g. .Settings → .homepage.SettingsHomepageActivity).
        final String pkgPrefix;
        int slash = component.indexOf('/');
        pkgPrefix = slash >= 0 ? component.substring(0, slash + 1) : component + "/";
        long elapsed = h.uiEvents.waits().awaitPredicate(new WaitRegistry.Predicate() {
            @Override public boolean check() {
                byte[] top = h.state.topActivity();
                if (top.length == 0 || (top.length >= 4 && top[0]=='E' && top[1]=='R' && top[2]=='R' && top[3]==':')) {
                    return false;
                }
                String s = new String(top, StandardCharsets.UTF_8);
                return s.equals(component) || s.startsWith(pkgPrefix);
            }
        }, timeout);
        if (elapsed < 0) return errBytes("TIMEOUT:wait_for_activity:" + component);
        return utf8("ok elapsed=" + elapsed);
    }

    private byte[] runTapAndDump(String cmd) {
        int x = (int) longArg(cmd, "x", -1);
        int y = (int) longArg(cmd, "y", -1);
        if (x < 0 || y < 0) return errBytes("tap_and_dump-needs-x-y");
        long idle    = longArg(cmd, "idle_ms", DEFAULT_IDLE_MS);
        long timeout = longArg(cmd, "timeout_ms", DEFAULT_TAP_TIMEOUT_MS);
        h.uiEvents.waits().touch();   // measure idle from after the tap
        h.input.tap(x, y);
        long el = h.uiEvents.waits().awaitIdle(idle, timeout);
        if (el < 0) return errBytes("TIMEOUT:tap_and_dump");
        try { return utf8(h.dumper.dumpActive()); }
        catch (Throwable t) { return errBytes("INTERNAL:dump-failed:" + t.getMessage()); }
    }

    private byte[] runTapAndSettle(String cmd) {
        int x = (int) longArg(cmd, "x", -1);
        int y = (int) longArg(cmd, "y", -1);
        if (x < 0 || y < 0) return errBytes("tap_and_settle-needs-x-y");
        long idle    = longArg(cmd, "idle_ms", DEFAULT_IDLE_MS);
        long timeout = longArg(cmd, "timeout_ms", DEFAULT_TAP_TIMEOUT_MS);
        h.uiEvents.waits().touch();   // measure idle from after the tap
        h.input.tap(x, y);
        long el = h.uiEvents.waits().awaitIdle(idle, timeout);
        if (el < 0) return errBytes("tap-then-timeout");
        return utf8("ok elapsed=" + el);
    }

    // ---------- node_* handlers ----------

    private byte[] runNodeClick(String cmd, boolean longClick) {
        String sel = afterHead(cmd);
        if (sel == null) return errBytes((longClick ? "node_long_click" : "node_click") + "-needs-selector");
        return longClick ? h.nodes.longClick(sel) : h.nodes.click(sel);
    }

    private byte[] runNodeSetText(String cmd) {
        String tail = afterHead(cmd);
        if (tail == null) return errBytes("node_set_text-needs-selector-and-value");
        // Pull value= out, treat the rest as the selector.
        String value = extractKey(tail, "value");
        if (value == null) return errBytes("node_set_text-needs-value=");
        // Empty selector is intentional: NodeActions.setText handles it by
        // targeting whichever EditText currently has input focus. Kept
        // explicit here so callers don't need a sentinel value — they just
        // omit the selector and `node_set_text value="..."` writes to the
        // focused field, which is what hs tui needs after a tap-to-focus.
        return h.nodes.setText(removeKey(tail, "value").trim(), value);
    }

    private byte[] runNodeScroll(String cmd) {
        String tail = afterHead(cmd);
        if (tail == null) return errBytes("node_scroll-needs-selector-and-dir");
        String dir = extractKey(tail, "dir");
        if (dir == null) dir = "forward";
        String sel = removeKey(tail, "dir");
        if (sel.isEmpty()) return errBytes("node_scroll-needs-selector");
        return h.nodes.scroll(sel, dir);
    }

    private byte[] runNodeFocus(String cmd) {
        String sel = afterHead(cmd);
        if (sel == null) return errBytes("node_focus-needs-selector");
        return h.nodes.focus(sel);
    }

    private byte[] runSubmit(String cmd) {
        // `submit` with no args targets the focused EditText. A trailing
        // selector overrides that — `submit text~=Email`.
        return h.nodes.imeAction(afterHead(cmd));
    }

    private byte[] runPaste(String cmd) {
        // `paste` with no args inserts the system clipboard into the
        // focused EditText. Trailing selector overrides — `paste id=...`.
        return h.nodes.pasteAction(afterHead(cmd));
    }

    private byte[] runDeeplinks(String cmd) {
        String pkg = positional(cmd);
        return h.manifest.deeplinks(pkg);
    }

    // ---------- user-data ContentProvider verbs ----------

    private byte[] runSms(String cmd) {
        String kind = strArgOr(cmd, "type", "inbox");
        int limit = (int) longArg(cmd, "limit", 50);
        return h.providers.sms(kind, limit);
    }

    private byte[] runCalls(String cmd) {
        String kind = strArgOr(cmd, "type", "all");
        int limit = (int) longArg(cmd, "limit", 50);
        return h.providers.calls(kind, limit);
    }

    private byte[] runContacts(String cmd) {
        int limit = (int) longArg(cmd, "limit", 50);
        return h.providers.contacts(limit);
    }

    private byte[] runCalendar(String cmd) {
        long now = System.currentTimeMillis();
        long from = longArg(cmd, "from", now);
        long to   = longArg(cmd, "to",   now + 7L * 24 * 60 * 60 * 1000);
        int limit = (int) longArg(cmd, "limit", 50);
        return h.providers.calendar(from, to, limit);
    }

    private byte[] runNotifications(String cmd) {
        String pkg = strArg(cmd, "pkg");
        int limit  = (int) longArg(cmd, "limit", 50);
        boolean history = longArg(cmd, "history", 0) != 0;
        return h.notifs.dump(pkg, limit, history);
    }

    private void runClipWatch(String cmd, java.io.DataOutputStream out) {
        long interval = longArg(cmd, "interval_ms", 500);
        h.clip.watch(out, interval);
    }

    // ---------- key=value arg helpers ----------

    private static NodeActions.Selector textSelector(String text, String mode) {
        NodeActions.Selector s = new NodeActions.Selector();
        if ("exact".equalsIgnoreCase(mode)) s.textExact = text;
        else                                s.textSub = text;
        return s;
    }

    /** Parse {@code key=value} from a whitespace-tokenised string. */
    private static String strArg(String cmd, String key) {
        return strArgOr(cmd, key, null);
    }

    private static String strArgOr(String cmd, String key, String dflt) {
        int sp = cmd.indexOf(' ');
        if (sp < 0) return dflt;
        String tail = cmd.substring(sp + 1);
        String pat = key + "=";
        int i = 0;
        while ((i = tail.indexOf(pat, i)) >= 0) {
            if (i == 0 || Character.isWhitespace(tail.charAt(i - 1))) {
                int start = i + pat.length();
                if (start < tail.length() && tail.charAt(start) == '"') {
                    int end = tail.indexOf('"', start + 1);
                    if (end > 0) return tail.substring(start + 1, end);
                    return dflt;
                }
                int end = start;
                while (end < tail.length() && !Character.isWhitespace(tail.charAt(end))) end++;
                return tail.substring(start, end);
            }
            i += pat.length();
        }
        return dflt;
    }

    private static long longArg(String cmd, String key, long dflt) {
        String v = strArg(cmd, key);
        if (v == null) return dflt;
        try { return Long.parseLong(v); }
        catch (NumberFormatException nf) { return dflt; }
    }

    /** Same as strArg but works on an arbitrary token string (no command head). */
    private static String extractKey(String tail, String key) {
        String fakeCmd = "_ " + tail;
        return strArg(fakeCmd, key);
    }

    /** Strip a {@code key="..."} or {@code key=value} token from {@code tail}. */
    private static String removeKey(String tail, String key) {
        StringBuilder out = new StringBuilder(tail.length());
        int i = 0;
        boolean inQuote = false;
        StringBuilder tok = new StringBuilder();
        while (i < tail.length()) {
            char c = tail.charAt(i);
            if (c == '"') inQuote = !inQuote;
            if (Character.isWhitespace(c) && !inQuote) {
                String t = tok.toString();
                if (!t.isEmpty()) {
                    if (!t.startsWith(key + "=")) {
                        if (out.length() > 0) out.append(' ');
                        out.append(t);
                    }
                    tok.setLength(0);
                }
            } else {
                tok.append(c);
            }
            i++;
        }
        String t = tok.toString();
        if (!t.isEmpty() && !t.startsWith(key + "=")) {
            if (out.length() > 0) out.append(' ');
            out.append(t);
        }
        return out.toString();
    }

    /**
     * Streaming subscription to State.device() snapshots. Server pushes one
     * frame whenever the cache is recomputed (driven by the existing a11y +
     * display listeners). The client side hands these straight to stdout /
     * its local mirror — no polling, no per-read binder hops.
     *
     * The connection thread blocks on the queue here. The State refresher
     * thread re-fires every HEARTBEAT_MS even if nothing changed, so a
     * write failure surfaces a dead client within that window.
     */
    private void runStateWatch(DataOutputStream out) {
        final java.util.concurrent.LinkedBlockingQueue<byte[]> q =
                new java.util.concurrent.LinkedBlockingQueue<>(8);
        final State.SnapshotListener listener = new State.SnapshotListener() {
            @Override public void onSnapshot(byte[] json) {
                if (!q.offer(json)) {
                    q.poll();          // drop oldest, keep latest
                    q.offer(json);
                }
            }
        };
        h.state.subscribe(listener);
        try {
            while (true) {
                byte[] frame;
                try { frame = q.take(); }
                catch (InterruptedException ie) { return; }
                try {
                    synchronized (out) {
                        out.writeInt(frame.length);
                        out.write(frame);
                        out.flush();
                    }
                } catch (IOException ioe) {
                    return;           // client gone
                }
            }
        } finally {
            h.state.unsubscribe(listener);
        }
    }

    private byte[] runState(String cmd) {
        String field = positional(cmd);
        if (field == null) return errBytes("state-needs-field");
        switch (field) {
            case "interactive":       return h.state.interactive();
            case "battery_level":     return h.state.batteryLevel();
            case "battery_charging":  return h.state.batteryCharging();
            case "top":               return h.state.topActivity();
            case "procs":             return h.state.procs();
            case "device":            return h.state.device();
            case "device_fresh":      return h.state.deviceFresh();
            default: return errBytes("unknown-state-field:" + field);
        }
    }

    /** Return everything after the command head (the first space-separated token), or null. */
    private static String afterHead(String cmd) {
        int sp = cmd.indexOf(' ');
        if (sp < 0) return null;
        String tail = cmd.substring(sp + 1).trim();
        return tail.isEmpty() ? null : tail;
    }

    /** Write an ERR frame then the chunked-stream EOF terminator. */
    private static void writeErrAndTerminator(DataOutputStream out, String tail) {
        try {
            byte[] msg = errBytes(tail);
            synchronized (out) {
                out.writeInt(msg.length);
                out.write(msg);
                out.writeInt(0);
                out.flush();
            }
        } catch (IOException ignored) {}
    }

    private static String[] positionalPair(String cmd) {
        int sp = cmd.indexOf(' ');
        if (sp < 0) return null;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return null;
        String[] parts = tail.split("\\s+", 2);
        if (parts.length < 2) return null;
        return parts;
    }
}
