package dev.peersus.app;

import android.app.Application;
import android.app.Service;
import android.content.Intent;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Message;
import android.os.Messenger;
import android.os.ParcelFileDescriptor;
import android.os.Process;
import android.os.RemoteException;
import java.io.IOException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;

/** Dedicated :store process: no native owner, plugin code, caller URLs or output paths. */
public final class StoreFetchService extends Service {
    static final int FETCH = 1, RESULT = 2, STOP = 3;
    private final ExecutorService io = Executors.newSingleThreadExecutor();
    private final AtomicBoolean busy = new AtomicBoolean();
    private final Messenger inbox = new Messenger(new Handler(Looper.getMainLooper(), this::receive));

    @Override public void onCreate() {
        super.onCreate();
        if (!workerProcess()) throw new IllegalStateException("The store worker requires its own process");
    }

    private boolean workerProcess() { return Application.getProcessName().equals(getPackageName() + ":store"); }
    private void exit() { if (workerProcess()) Process.killProcess(Process.myPid()); }
    @Override public IBinder onBind(Intent intent) { return inbox.getBinder(); }
    @Override public void onDestroy() { io.shutdownNow(); super.onDestroy(); exit(); }

    private boolean receive(Message message) {
        if (message.sendingUid != Process.myUid()) return true;
        if (message.what == STOP) { exit(); return true; }
        if (message.what != FETCH) return true;
        ParcelFileDescriptor output = Parcelables.get(message.getData(), "output", ParcelFileDescriptor.class);
        Messenger answer = message.replyTo;
        String name = message.getData().getString("name");
        long limit = message.getData().getLong("limit", -1);
        if (output == null || answer == null || limit < 0 || limit > StoreDownload.MAX_BYTES
                || !busy.compareAndSet(false, true)) {
            close(output);
            answer(answer, false);
            return true;
        }
        io.execute(() -> {
            boolean ok = false;
            try {
                try (var stream = new ParcelFileDescriptor.AutoCloseOutputStream(output)) {
                    PrivateStoreConfig.downloader(this).copy(name, limit, stream);
                    stream.flush();
                }
                ok = true;
            } catch (IOException | RuntimeException refused) {
                // Bytes, URLs and filesystem details never enter diagnostics.
            } finally {
                busy.set(false);
                answer(answer, ok);
            }
        });
        return true;
    }

    private static void answer(Messenger target, boolean ok) {
        if (target == null) return;
        Message reply = Message.obtain(null, RESULT, ok ? 1 : 0, Process.myPid());
        try { target.send(reply); } catch (RemoteException unavailable) { }
    }
    private static void close(ParcelFileDescriptor descriptor) {
        if (descriptor != null) try { descriptor.close(); } catch (IOException ignored) { }
    }
}
