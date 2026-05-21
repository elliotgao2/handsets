package dev.handsets.daemon;

import android.view.accessibility.AccessibilityEvent;

/**
 * Tracks the timestamp of the last UiAutomation event and exposes
 * {@link #awaitIdle} for "wait until N ms of quiet" and {@link #awaitPredicate}
 * for "block until this predicate is true, recheck on each event".
 *
 * Fed by {@link UiEvents} on every accessibility event.
 */
final class WaitRegistry {

    private final Object lock = new Object();
    private volatile long lastEventTs = System.currentTimeMillis();

    void onEvent(AccessibilityEvent ev) {
        // The actual content of the event doesn't matter here — any event
        // counts as "the UI is still settling".
        touch();
    }

    /**
     * Manually flag "an action just happened" so the next {@code awaitIdle}
     * measures from now, not from whatever stale a11y event preceded the
     * action. Composite ops (tap_and_*) call this before they tap so the
     * tap's resulting events are guaranteed to fall inside the idle window.
     */
    void touch() {
        synchronized (lock) {
            lastEventTs = System.currentTimeMillis();
            lock.notifyAll();
        }
    }

    /** @return millis elapsed since the event stream went idle, or -1 on timeout. */
    long awaitIdle(long idleMs, long timeoutMs) {
        long deadline = System.currentTimeMillis() + timeoutMs;
        synchronized (lock) {
            while (true) {
                long now = System.currentTimeMillis();
                long since = now - lastEventTs;
                if (since >= idleMs) return since;
                if (now >= deadline) return -1;
                long waitFor = Math.min(idleMs - since, deadline - now);
                try { lock.wait(waitFor); }
                catch (InterruptedException ie) { return -1; }
            }
        }
    }

    /** Generic predicate-based wait: re-evaluate {@code p} on every event. */
    interface Predicate { boolean check(); }

    /** @return elapsed ms when the predicate first became true, or -1 on timeout. */
    long awaitPredicate(Predicate p, long timeoutMs) {
        long start = System.currentTimeMillis();
        long deadline = start + timeoutMs;
        if (safeCheck(p)) return 0;
        synchronized (lock) {
            while (true) {
                long now = System.currentTimeMillis();
                if (now >= deadline) return -1;
                try { lock.wait(deadline - now); }
                catch (InterruptedException ie) { return -1; }
                if (safeCheck(p)) return System.currentTimeMillis() - start;
            }
        }
    }

    private static boolean safeCheck(Predicate p) {
        try { return p.check(); }
        catch (Throwable t) { return false; }
    }
}
