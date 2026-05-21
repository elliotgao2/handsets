package dev.handsets.daemon;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.UiAutomation;
import android.view.accessibility.AccessibilityEvent;

import java.util.concurrent.CopyOnWriteArrayList;

/**
 * Single fan-out point for {@code UiAutomation.OnAccessibilityEventListener}.
 *
 * {@code UiAutomation.setOnAccessibilityEventListener} only accepts one
 * listener; if {@link State} and the wait-for-* primitives both registered
 * separately the second one would clobber the first. So we own the
 * registration here and dispatch each event to every interested consumer:
 *
 * <ul>
 *   <li>{@link State} — invalidates its device-snapshot cache on
 *       {@code TYPE_WINDOW_STATE_CHANGED} / {@code TYPE_WINDOWS_CHANGED}.</li>
 *   <li>{@link WaitRegistry} — wakes any active {@code wait_for_idle} /
 *       {@code wait_for_text} / {@code wait_for_activity} waiter.</li>
 * </ul>
 */
final class UiEvents {

    interface Consumer {
        void onEvent(AccessibilityEvent ev);
    }

    private final UiAutomation ua;
    private final WaitRegistry waits = new WaitRegistry();
    private final CopyOnWriteArrayList<Consumer> consumers = new CopyOnWriteArrayList<>();
    private volatile boolean installed;

    UiEvents(UiAutomation ua) {
        this.ua = ua;
        // The wait registry is always-on so we route every event to it.
        consumers.add(new Consumer() {
            @Override public void onEvent(AccessibilityEvent ev) { waits.onEvent(ev); }
        });
    }

    WaitRegistry waits() { return waits; }

    void subscribe(Consumer c) {
        consumers.add(c);
        install();
    }

    /** Idempotent: only the first subscribe actually attaches the listener. */
    private synchronized void install() {
        if (installed) return;
        try {
            AccessibilityServiceInfo info = ua.getServiceInfo();
            if (info != null) {
                info.eventTypes |= AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED
                                | AccessibilityEvent.TYPE_WINDOWS_CHANGED
                                | AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED;
                ua.setServiceInfo(info);
            }
            ua.setOnAccessibilityEventListener(new UiAutomation.OnAccessibilityEventListener() {
                @Override public void onAccessibilityEvent(AccessibilityEvent ev) {
                    for (Consumer c : consumers) {
                        try { c.onEvent(ev); } catch (Throwable ignored) {}
                    }
                }
            });
            installed = true;
        } catch (Throwable t) {
            System.err.println("warn: UiEvents install failed: " + t);
        }
    }
}
