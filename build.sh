#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
BT="${BT:-$SDK/build-tools/35.0.1}"
PLAT="${PLAT:-$SDK/platforms/android-35/android.jar}"

if [[ ! -d "$BT" ]]; then
  echo "error: build-tools not found at $BT" >&2
  exit 1
fi
if [[ ! -f "$PLAT" ]]; then
  echo "error: platform jar not found at $PLAT" >&2
  exit 1
fi

rm -rf build
mkdir -p build/classes build/dex

find src -name '*.java' > build/sources.txt
javac -source 1.8 -target 1.8 \
      -Xlint:-options \
      -bootclasspath "$PLAT" \
      -d build/classes \
      @build/sources.txt

# Collect compiled classes for dex'ing.
find build/classes -name '*.class' > build/classes.txt

# Prefer R8 (shrinking + minification → ~13% smaller dex than plain d8).
# Falls back to d8 if R8 isn't on this machine.
R8_BIN="$SDK/cmdline-tools/latest/bin/r8"
if [[ -x "$R8_BIN" ]]; then
  cat > build/r8.pro <<'PRO'
# Entry point: app_process invokes Main.main.
-keep public class dev.handsets.daemon.Main {
    public static void main(java.lang.String[]);
}
# Allow R8 to rename everything else and re-pack to a single root namespace.
-allowaccessmodification
-repackageclasses ''
# We never inspect class names at runtime (only framework class names via
# reflection, which R8 leaves alone), so renames are safe. Anonymous inner
# classes that implement framework interfaces (OnImageAvailableListener,
# FrameListener, Runnable) keep their override method names automatically.
-dontwarn **
PRO
  SKIP_JDK_VERSION_CHECK=1 "$R8_BIN" \
      --release \
      --pg-conf build/r8.pro \
      --output build/dex \
      --lib "$PLAT" \
      --min-api 28 \
      $(cat build/classes.txt)
else
  echo "(R8 not found at $R8_BIN — falling back to d8; jar will be slightly larger)"
  xargs "$BT/d8" --output build/dex --lib "$PLAT" --min-api 28 < build/classes.txt
fi

( cd build/dex && jar cf ../hs.jar classes.dex )

ls -lh build/hs.jar
