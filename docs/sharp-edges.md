# Sharp edges

- Settings provider rejects our `app_process` identity; `SettingsDirect`
  uses `IActivityManager.getContentProviderExternal` (the `cmd content`
  path).
- `am start` goes via `IActivityTaskManager.startActivityAsUser` with
  `callingPackage="com.android.shell"`; the system Context can't host
  `startActivity` from our anonymous process.
- `BroadcastReceiver` registrations from our Context aren't routed —
  `appCounts` cache TTL'd at 4.5 s instead.
- Most binder lookups go via reflection so the jar compiles against the
  public SDK. If a new SDK level renames a method, widen the matcher in
  the relevant helper.
