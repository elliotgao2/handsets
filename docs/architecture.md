# Architecture

```
host                                  device (shell UID, app_process)
─────                                  ──────────────────────────────
hs <verb> ──► TCP forward ────►     Server.java
                                       ├─ State            (push-cached snapshot)
                                       ├─ Dumper Screenshot Input
                                       ├─ Files Installer  (chunked streaming)
                                       ├─ Pm Am Wm Props   (direct binder)
                                       ├─ SettingsDirect   (IContentProvider via External)
                                       ├─ Dumpsys Logcat ShellExec Lifecycle
                                       ├─ UiEvents WaitRegistry  (wait_for_*)
                                       └─ NodeActions      (AccessibilityNodeInfo)
```

The daemon runs as the shell UID via `app_process` with hidden-API
restrictions lifted. The host runs a background `state-daemon` that
subscribes to `state_watch` and atomically rewrites
`~/.handsets/state-<port>.json` on every event-driven refresh. `hs info` /
`hs show` / `hs state X` all read straight out of that file.

## Layout

```
src/dev/handsets/daemon/      on-device daemon (Java → dex → jar)
handsets-cli/                 host CLI — short-verb surface (`hs use`, `hs see`, …)
handsets-viewer/              GUI mirror — winit + Metal + zero-copy VideoToolbox
build.sh                      javac → R8 → dex → jar
```
