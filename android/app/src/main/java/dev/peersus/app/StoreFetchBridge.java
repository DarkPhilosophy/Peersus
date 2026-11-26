package dev.peersus.app;

import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Message;
import android.os.Messenger;
import android.os.ParcelFileDescriptor;
import android.os.Process;
import android.os.RemoteException;
import java.io.File;
import java.io.IOException;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/** Runs only on the owner's blocking pool. The main and async owner loops never wait here. */
final class StoreFetchBridge {
    static boolean download(Context context, String name, long limit, String output) {
        if (Looper.myLooper() == Looper.getMainLooper() || output == null || limit < 0
                || limit > StoreDownload.MAX_BYTES) return false;
        try (ParcelFileDescriptor descriptor = ParcelFileDescriptor.open(new File(output),
                ParcelFileDescriptor.MODE_WRITE_ONLY | ParcelFileDescriptor.MODE_TRUNCATE)) {
            Session session = new Session(context.getApplicationContext(), name, limit, descriptor);
            try {
                session.bound = session.context.bindService(new Intent(context, StoreFetchService.class), session,
                        Context.BIND_AUTO_CREATE);
                if (!session.bound) return false;
                // Also bounds a peer trickling HTTP headers forever.
                return session.finished.await(75, TimeUnit.SECONDS) && session.ok.get();
            } finally {
                session.close();
            }
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return false;
        } catch (IOException | RuntimeException unavailable) {
            return false;
        }
    }

    private static final class Session implements ServiceConnection {
        final Context context;
        final String name;
        final long limit;
        final ParcelFileDescriptor output;
        final CountDownLatch finished = new CountDownLatch(1);
        final AtomicBoolean ok = new AtomicBoolean();
        final AtomicBoolean live = new AtomicBoolean(true);
        final Messenger inbox;
        volatile Messenger remote;
        boolean bound;

        Session(Context context, String name, long limit, ParcelFileDescriptor output) {
            this.context = context; this.name = name; this.limit = limit; this.output = output;
            inbox = new Messenger(new Handler(Looper.getMainLooper(), message -> {
                if (live.get() && message.what == StoreFetchService.RESULT && message.sendingUid == Process.myUid()
                        && message.arg2 > 0 && message.arg2 != Process.myPid()) {
                    ok.set(message.arg1 == 1);
                    finished.countDown();
                }
                return true;
            }));
        }

        @Override public void onServiceConnected(ComponentName component, IBinder service) {
            Messenger worker = new Messenger(service);
            remote = worker;
            if (!live.get()) { stop(worker); return; }
            try {
                Message request = Message.obtain(null, StoreFetchService.FETCH);
                request.replyTo = inbox;
                request.getData().putString("name", name);
                request.getData().putLong("limit", limit);
                request.getData().putParcelable("output", output);
                worker.send(request);
            } catch (RemoteException | RuntimeException unavailable) {
                finished.countDown();
            }
        }
        @Override public void onServiceDisconnected(ComponentName component) { finished.countDown(); }
        @Override public void onBindingDied(ComponentName component) { finished.countDown(); }
        @Override public void onNullBinding(ComponentName component) { finished.countDown(); }

        void close() {
            live.set(false);
            Messenger worker = remote;
            if (worker != null) stop(worker);
            if (bound) {
                bound = false;
                try { context.unbindService(this); } catch (RuntimeException unavailable) { }
            }
        }
        private static void stop(Messenger worker) {
            try { worker.send(Message.obtain(null, StoreFetchService.STOP)); }
            catch (RemoteException | RuntimeException unavailable) { }
        }
    }
}
