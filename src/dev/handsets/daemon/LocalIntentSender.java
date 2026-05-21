package dev.handsets.daemon;

import android.content.Intent;
import android.content.IntentSender;
import android.os.Binder;
import android.os.IBinder;
import android.os.Parcel;
import android.os.RemoteException;

import java.lang.reflect.Constructor;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

/**
 * In-process Binder that fulfils the {@code IIntentSender} contract just
 * enough to capture the Intent that {@link android.content.pm.PackageInstaller}
 * uses to report install / uninstall results.
 *
 * Transaction code 1 (IIntentSender.send) has been stable across every API
 * level we target, and the first two AIDL arguments (int sendCode, Intent
 * intent) have not changed — we don't care about the trailing arguments that
 * have shifted with new platform versions.
 */
final class LocalIntentSender extends Binder {

    private final LinkedBlockingQueue<Intent> q = new LinkedBlockingQueue<>(4);

    /** Build a public {@link IntentSender} backed by this Binder. */
    IntentSender intentSender() throws Exception {
        Constructor<IntentSender> ctor = IntentSender.class.getDeclaredConstructor(IBinder.class);
        ctor.setAccessible(true);
        return ctor.newInstance(this);
    }

    /** Block for an Intent, or null on timeout. */
    Intent await(long ms) throws InterruptedException {
        return q.poll(ms, TimeUnit.MILLISECONDS);
    }

    @Override
    protected boolean onTransact(int code, Parcel data, Parcel reply, int flags)
            throws RemoteException {
        if (code == 1) {
            try { data.enforceInterface("android.content.IIntentSender"); }
            catch (Throwable ignored) {}
            Intent intent = null;
            try {
                data.readInt();   // sendCode
                if (data.readInt() != 0) {
                    intent = Intent.CREATOR.createFromParcel(data);
                }
            } catch (Throwable ignored) {}
            q.offer(intent != null ? intent : new Intent());
            if (reply != null) reply.writeNoException();
            return true;
        }
        return super.onTransact(code, data, reply, flags);
    }
}
