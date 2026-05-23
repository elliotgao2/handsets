// `--help` text for the `hs` binary.
//
// Lives in its own module so main.rs can stay focused on argument parsing
// and dispatch. The 100-line USAGE constant used to dominate the top of
// main.rs and made it hard to skim the verb table next to the dispatcher
// that consumes it.

pub const USAGE: &str = "\
hs — drive Android from the shell. Built for LLM agents and humans.

Usage: hs [--device SERIAL] [--json] <verb> [args]

The agent loop:
  hs use                       connect a device, start the daemon
  hs ui                        flat table of tappable nodes (the LLM input format)
  hs tap \"Continue\"             find by text, tap centre
  hs type TEXT                 type into the focused field
  hs wait \"Welcome\"             wait for that text to appear
  hs drop                      tear the daemon down

Selectors (CSS-like, Playwright-inspired):
  hs find 'Button[text=\"OK\"]'             attribute predicates
  hs find 'Button:has-text(\"Sign in\")'    Playwright sugar for [text~=]
  hs find '*EditText:below(TextView[text=Email])'
  hs find 'Button:near(ImageView[desc~=cart], 200)'
  Pseudos: :visible :clickable :enabled :focused :checkable :checked
           :has-text(\"x\") :text-is(\"x\")
           :in(SEL) :below(SEL) :right-of(SEL) :near(SEL, PX)

Shared action flags (tap, type, find, wait, submit, paste, act):
  --timeout MS         per-call wait budget (default 10 s)
  --retries N          retry on TIMEOUT / NOT_FOUND  (with --retry-delay MS)
  --visible            require isVisibleToUser
  --clickable          require framework-clickable
  --enabled            require enabled
  --unique             fail with AMBIGUOUS if >1 match
  --nth I              pick the I-th match (1-indexed)
  --json               emit {\"verb\":…, \"ok\":…, \"result\"|\"error\":…} per line

More verbs:
  Capture     hs see [PATH.jpg|.png|.xml|.json]
              hs ui [-i|--tree|--json|--xml] [--all]
              hs info | hs show [top|PKG]
  Input       hs tap X Y | hs swipe DIR | hs go back|home|recents|…
              hs type TEXT                  keystrokes to focused field
              hs fill SELECTOR TEXT         atomic ACTION_SET_TEXT
              hs submit | hs paste
  Lifecycle   hs open PKG[/.Class] | hs close PKG
              hs install APK… | hs uninstall PKG
  Files       hs cp device:src dst | src device:dst
  Apps        hs apps [--3rd] | hs links PKG
  Data        hs sms | hs calls | hs contacts | hs calendar | hs notif | hs clip
  System      hs prop [KEY [VAL]] | hs settings [NS [KEY [VAL]]]
  Diagnostics hs logs [--tail N | --follow] | hs events
  Scripting   hs shell                 interactive REPL (also batch via stdin)
              hs run [SCRIPT|-]        batch CLI verbs over one warm socket
              hs init [PATH]           scaffold a starter script
              hs act --tap … --until … one-shot tap-then-verify composite
              hs fan SERIAL,SERIAL -- VERB    parallel per-device

Exit codes:
  0  ok
  1  failure (everything below not broken-out — see error.code in --json output)
  2  NOT_FOUND      no node matched
  3  TIMEOUT        wait budget exhausted
  4  AMBIGUOUS      --unique saw multiple matches

Global options:
  --host HOST        default 127.0.0.1
  --port PORT        default 9008
  --device, -s SERIAL  route to the daemon for SERIAL
  --json             default output to JSON (or set HS_FORMAT=json)

Low-level / debugging:
  hs dev <sub>       ping | snapshot | screen | bench | quit | state-daemon
  hs do <wire>       fire one raw wire command (see docs/wire.md)
";
