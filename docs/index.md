# Handsets

*A millisecond-latency CLI for driving Android devices. Built for LLM agents and shell scripts.*

- **Fast** — 2–7 ms per call; the daemon stays warm and state is mirrored to a host file for µs reads.
- **Agent-shaped** — `hs ui -i` returns a flat table of tappable nodes (~10× fewer tokens than XML).
- **No app, no root** — one small jar pushed to the device; runs under shell UID via `app_process`.

[Install from GitHub →](https://github.com/elliotgao2/handsets){ .md-button .md-button--primary }

---

## The agent loop

```bash
$ hs use                              # auto-detects device, starts the daemon
daemon up on tcp:9008

$ hs ui -i                            # flat list of tappable nodes — drop into an LLM
@(540,540)   click             EditText    #email        desc="Email"
@(540,640)   click,password    EditText    #password     desc="Password"
@(540,860)   click             Button      #continue     "Continue"

$ hs tap "Continue"                   # text-lookup tap → coords → ACTION_CLICK
tapped "Continue" cls=android.widget.Button → ok
```

Drop `hs ui -i` into an LLM, get back a label, hand it to `hs tap` — that's the loop.

## Where to next

- **[Architecture](architecture.md)** — how the daemon, mirror, and wire fit together
- **[Cookbook](cookbook.md)** — RPA recipes (login, retry, fan-out)
- **[Wire reference](wire.md)** — raw protocol
- **[Sharp edges](sharp-edges.md)** — known gotchas
- **[Benchmark](benchmark.md)** — full latency numbers
- **[Blog](blog/index.md)** — long-form posts on how Handsets works
