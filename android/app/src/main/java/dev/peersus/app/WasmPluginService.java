package dev.peersus.app;

import android.app.Service;
import android.content.Intent;
import android.os.Binder;
import android.os.Bundle;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Message;
import android.os.Messenger;
import android.os.Parcel;
import android.os.ParcelFileDescriptor;
import android.os.Process;
import android.os.RemoteException;
import java.io.IOException;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicLong;

/** Interpreter only, one isolated AMS instance per generation; no application UID permissions. */
public final class WasmPluginService extends Service {
    static { System.loadLibrary("peersus_plugin_android"); }
    private final Messenger inbox = new Messenger(new Handler(Looper.getMainLooper(), this::receive));
    private final ConcurrentHashMap<Long, Pending> pending = new ConcurrentHashMap<>();
    private final AtomicLong sequence = new AtomicLong(1);
    private Messenger owner;
    private IBinder ownerBinder;
    private long generation;
    private int ownerUid;
    private boolean started;
    private final IBinder.DeathRecipient death = () -> terminate();
    private static final class Pending {
        final CountDownLatch ready = new CountDownLatch(1);
        final long deadline = android.os.SystemClock.elapsedRealtime() + 2000;
        ParcelFileDescriptor descriptor;
        boolean cancelled;
    }

    @Override public IBinder onBind(Intent intent) {
        if (!isWorkerProcess() || started || intent == null) {
            android.util.Log.w("PeersusModules", Process.isIsolated() ? "Unexpected isolated process label" : "Worker is not isolated");
            return null;
        }
        generation = intent.getLongExtra("generation", 0);
        ownerUid = getApplicationInfo().uid;
        return generation > 0 ? inbox.getBinder() : null;
    }

    public boolean isWorkerProcess() {
        return Process.isIsolated() && android.app.Application.getProcessName().contains(":wasm");
    }

    private boolean receive(Message message) {
        if (message.sendingUid != ownerUid || message.getData().getLong("generation", 0) != generation) {
            closePayload(message.getData());
            return true;
        }
        if (message.what == ModuleBridge.STOP) { terminate(); return true; }
        if (message.what == ModuleBridge.DATA_RESULT) {
            Bundle values = message.getData();
            ParcelFileDescriptor descriptor = Parcelables.get(values, "descriptor", ParcelFileDescriptor.class);
            Pending request = pending.get(values.getLong("request", 0));
            if (request == null) { close(descriptor); return true; }
            synchronized (request) {
                if (request.cancelled || request.descriptor != null || android.os.SystemClock.elapsedRealtime() >= request.deadline) close(descriptor);
                else { request.descriptor = descriptor; request.ready.countDown(); }
            }
            return true;
        }
        if (message.what != ModuleBridge.ATTACH || started || message.replyTo == null) {
            closePayload(message.getData());
            return true;
        }
        started = true;
        owner = message.replyTo;
        ownerBinder = owner.getBinder();
        try { ownerBinder.linkToDeath(death, 0); }
        catch (RemoteException dead) { closePayload(message.getData()); terminate(); return true; }
        Bundle values = message.getData();
        ParcelFileDescriptor module = Parcelables.get(values, "module", ParcelFileDescriptor.class);
        ParcelFileDescriptor control = Parcelables.get(values, "control", ParcelFileDescriptor.class);
        String token = values.getString("token");
        if (module == null || control == null || token == null || !token.matches("[0-9a-f]{64}")) {
            close(module); close(control); terminate(); return true;
        }
        try {
            Message identity = Message.obtain(null, ModuleBridge.IDENTITY);
            identity.arg1 = Process.myPid();
            identity.arg2 = Process.myUid();
            identity.getData().putLong("generation", generation);
            owner.send(identity);
        } catch (RemoteException dead) { close(module); close(control); terminate(); return true; }
        new Thread(() -> {
            int moduleFd = -1, controlFd = -1;
            try {
                moduleFd = module.detachFd(); controlFd = control.detachFd();
                int ownedModule = moduleFd, ownedControl = controlFd;
                moduleFd = controlFd = -1;
                int result = nativeRun(ownedModule, ownedControl, token);
                try {
                    Message exit = Message.obtain(null, ModuleBridge.EXIT);
                    exit.arg1 = result;
                    exit.getData().putLong("generation", generation);
                    owner.send(exit);
                } catch (RemoteException ignored) {}
            } finally {
                if (moduleFd >= 0) close(ParcelFileDescriptor.adoptFd(moduleFd));
                if (controlFd >= 0) close(ParcelFileDescriptor.adoptFd(controlFd));
                close(module); close(control); terminate();
            }
        }, "wasm-extension").start();
        return true;
    }

    /** Called only by the native worker. Binder carries connected FDs, never socket paths. */
    public int openData() {
        if (Looper.myLooper() == Looper.getMainLooper() || owner == null || pending.size() >= 1) return -1;
        long id = sequence.getAndIncrement();
        if (id <= 0) return -1;
        Pending request = new Pending();
        pending.put(id, request);
        Binder lease = new Binder() {
            @Override protected boolean onTransact(int code, Parcel input, Parcel output, int flags) {
                if (code != IBinder.FIRST_CALL_TRANSACTION || Binder.getCallingUid() != ownerUid) return false;
                synchronized (request) {
                    output.writeInt(!request.cancelled && android.os.SystemClock.elapsedRealtime() < request.deadline ? 1 : 0);
                }
                return true;
            }
        };
        try {
            Message open = Message.obtain(null, ModuleBridge.OPEN_DATA);
            Bundle values = open.getData();
            values.putLong("generation", generation);
            values.putLong("request", id);
            values.putBinder("permit", lease);
            owner.send(open);
            long remaining = request.deadline - android.os.SystemClock.elapsedRealtime();
            if (remaining > 0 && request.ready.await(remaining, TimeUnit.MILLISECONDS)) {
                synchronized (request) {
                    if (!request.cancelled && android.os.SystemClock.elapsedRealtime() < request.deadline && request.descriptor != null) {
                        int fd = request.descriptor.detachFd();
                        request.descriptor = null;
                        return fd;
                    }
                }
            }
        } catch (RemoteException | InterruptedException unavailable) {
            if (unavailable instanceof InterruptedException) Thread.currentThread().interrupt();
        } finally {
            pending.remove(id);
            synchronized (request) { request.cancelled = true; close(request.descriptor); request.descriptor = null; }
        }
        return -1;
    }

    private static void closePayload(Bundle values) {
        close(Parcelables.get(values, "module", ParcelFileDescriptor.class));
        close(Parcelables.get(values, "control", ParcelFileDescriptor.class));
        close(Parcelables.get(values, "descriptor", ParcelFileDescriptor.class));
    }
    private static void close(ParcelFileDescriptor descriptor) {
        if (descriptor != null) try { descriptor.close(); } catch (IOException ignored) {}
    }
    private static void terminate() { if (Process.isIsolated()) Process.killProcess(Process.myPid()); }
    @Override public boolean onUnbind(Intent intent) { terminate(); return false; }
    @Override public void onDestroy() { super.onDestroy(); terminate(); }
    private native int nativeRun(int module, int control, String token);
}
