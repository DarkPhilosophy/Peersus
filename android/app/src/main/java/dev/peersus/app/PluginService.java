package dev.peersus.app;

import android.app.Service;
import android.content.Intent;
import android.os.Bundle;
import android.os.Binder;
import android.os.Parcel;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Message;
import android.os.Messenger;
import android.os.Process;
import android.os.RemoteException;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/** Bundled code only. The manifest MUST place this non-exported Service in :plugins. */
public final class PluginService extends Service {
    static { System.loadLibrary("peersus_android"); }
    private final ConcurrentHashMap<Long, Pending> pending = new ConcurrentHashMap<>();
    private final Messenger inbox = new Messenger(new Handler(Looper.getMainLooper(), this::receive));
    private volatile Messenger owner;
    private boolean started;
    private long generation;
    private static long activeGeneration;
    private static boolean nativeActive;
    private String socket, dataSocket, token;

    private static final class Pending {
        final CountDownLatch arrived = new CountDownLatch(1);
        final ClipboardRequest lease;
        final Binder permit;
        Pending(long deadline) {
            lease = new ClipboardRequest(deadline);
            permit = new Binder() {
                @Override protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) {
                    if (code != IBinder.FIRST_CALL_TRANSACTION) return false;
                    reply.writeInt(lease.allowed(android.os.SystemClock.elapsedRealtime()) ? 1 : 0);
                    return true;
                }
            };
        }
        volatile String text;
        volatile boolean ok, changed;
    }

    @Override public IBinder onBind(Intent intent) {
        if (!isPluginProcess() || intent == null) return null;
        if (!"clipboard".equals(intent.getStringExtra("entry"))) return null;
        long requested = intent.getLongExtra("generation", 0);
        if (requested <= 0) return null;
        // Never reuse a process containing an older native runtime.
        if (nativeActive) { terminate(); return null; }
        generation = activeGeneration = requested;
        socket = intent.getStringExtra("socket");
        dataSocket = intent.getStringExtra("dataSocket");
        token = intent.getStringExtra("token");
        if (socket == null || dataSocket == null || token == null || !socket.startsWith("/")
                || !dataSocket.startsWith("/") || socket.length() > 4096 || dataSocket.length() > 4096
                || token.isEmpty() || token.length() > 256) return null;
        return inbox.getBinder();
    }

    private boolean receive(Message message) {
        if (message.sendingUid != Process.myUid()) return true;
        if (message.getData().getLong("generation", 0) != generation) return true;
        if (message.what == PluginBridge.STOP) { terminate(); return true; }
        if (message.what == PluginBridge.ATTACH && !started && message.replyTo != null) {
            started = true;
            nativeActive = true;
            owner = message.replyTo;
            try {
                Message identity = Message.obtain(null, PluginBridge.PID);
                identity.arg1 = Process.myPid();
                identity.getData().putLong("generation", generation);
                owner.send(identity);
            } catch (RemoteException dead) { terminate(); return true; }
            final String control = socket, data = dataSocket, secret = token;
            socket = dataSocket = token = null;
            new Thread(() -> {
                try { nativeRun(control, data, secret); }
                finally { terminate(); }
            }, "bundled-clipboard").start();
        } else if (message.what == PluginBridge.RESULT) {
            Bundle result = message.getData();
            Pending request = pending.get(result.getLong("request", 0));
            if (request != null) {
                String text = result.getString("text");
                request.text = ClipboardAccess.bounded(text) ? text : null;
                request.ok = result.getBoolean("ok", false);
                request.changed = result.getBoolean("changed", false);
                request.arrived.countDown();
            }
        }
        return true;
    }

    // Registered before the blocking worker is spawned. Dropping its Rust future cancels
    // this exact object even if that worker has not started yet.
    public boolean beginClipboardRequest(long id, long remainingMillis) {
        if (id <= 0 || remainingMillis <= 0 || remainingMillis > 2500 || pending.size() >= 8) return false;
        Pending request = new Pending(android.os.SystemClock.elapsedRealtime() + remainingMillis);
        return pending.putIfAbsent(id, request) == null;
    }

    public void cancelClipboardRequest(long id) {
        Pending request = pending.remove(id);
        if (request != null) {
            request.lease.cancel();
            request.arrived.countDown();
        }
    }

    private Pending request(int operation, String text, boolean observe, long id, long context) {
        Messenger target = owner;
        Pending answer = pending.get(id);
        if (target == null || answer == null || Looper.myLooper() == Looper.getMainLooper()
                || !answer.lease.allowed(android.os.SystemClock.elapsedRealtime())) return null;
        try {
            Message message = Message.obtain(null, operation);
            Bundle values = new Bundle();
            values.putLong("request", id);
            values.putLong("generation", generation);
            values.putLong("deadline", answer.lease.deadline);
            values.putBinder("permit", answer.permit);
            values.putLong("context", context);
            if (operation == PluginBridge.WRITE) values.putString("text", text);
            values.putBoolean("observe", observe);
            message.setData(values);
            target.send(message);
            long remaining = answer.lease.deadline - android.os.SystemClock.elapsedRealtime();
            return remaining > 0 && answer.arrived.await(remaining, TimeUnit.MILLISECONDS)
                && answer.lease.allowed(android.os.SystemClock.elapsedRealtime()) ? answer : null;
        } catch (RemoteException dead) {
            terminate();
            return null;
        } catch (InterruptedException cancelled) {
            Thread.currentThread().interrupt();
            return null;
        } finally { cancelClipboardRequest(id); }
    }

    // Called exclusively on native blocking workers, never the main/Binder thread.
    public String readClipboard(long id) {
        Pending result = request(PluginBridge.READ, null, false, id, 0);
        return result == null ? null : result.text;
    }

    public String selectedClipboard(long id, long context) {
        Pending result = request(PluginBridge.SELECTED, null, false, id, context);
        return result == null ? null : result.text;
    }

    public String observeClipboard(long id) {
        Pending result = request(PluginBridge.READ, null, true, id, 0);
        return result == null || result.text == null ? null : (result.changed ? "c" : "s") + result.text;
    }

    public boolean selectedClipboardValid(long id, long context) {
        Pending result = request(PluginBridge.CHECK_SELECTED, null, false, id, context);
        return result != null && result.ok;
    }

    public boolean resetClipboard(long id) {
        Pending result = request(PluginBridge.RESET, null, false, id, 0);
        return result != null && result.ok;
    }

    public boolean writeClipboard(String text, long id) {
        if (!ClipboardAccess.bounded(text)) return false;
        Pending result = request(PluginBridge.WRITE, text, false, id, 0);
        return result != null && result.ok;
    }

    public boolean isPluginProcess() {
        return android.app.Application.getProcessName().equals(getPackageName() + ":plugins");
    }

    private static void terminate() {
        if (android.app.Application.getProcessName().endsWith(":plugins")) Process.killProcess(Process.myPid());
    }
    @Override public boolean onUnbind(Intent intent) {
        if (nativeActive && intent.getLongExtra("generation", 0) == activeGeneration) terminate();
        return false;
    }
    @Override public void onDestroy() {
        super.onDestroy();
        if (nativeActive && generation == activeGeneration) terminate();
    }
    private native void nativeRun(String socket, String dataSocket, String token);
}
