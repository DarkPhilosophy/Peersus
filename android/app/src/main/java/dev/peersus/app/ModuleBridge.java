package dev.peersus.app;

import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
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
import java.io.File;
import java.io.IOException;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;

/** Moves descriptors only. Existing broker Hello, data tickets and grants remain authoritative. */
final class ModuleBridge {
    static final int ATTACH = 101, STOP = 102, IDENTITY = 103, OPEN_DATA = 104, DATA_RESULT = 105, EXIT = 106;
    private static final Handler main = new Handler(Looper.getMainLooper());
    private static final ExecutorService io = Executors.newFixedThreadPool(2);
    private static final Map<Long, Session> sessions = new HashMap<>();
    private enum SocketKind { CONTROL, DATA, CANVAS }

    static boolean launch(Context context, long generation, String module, String socket, String dataSocket, String token) {
        if (generation <= 0 || module == null || socket == null || dataSocket == null || token == null
                || !module.startsWith("/") || !socket.startsWith("/") || !dataSocket.startsWith("/")
                || module.length() > 4096 || socket.length() > 4096 || dataSocket.length() > 4096
                || !token.matches("[0-9a-f]{64}")) return false;
        return main.post(() -> {
            if (sessions.containsKey(generation) || sessions.size() >= 64) { PluginBridge.moduleExited(generation); return; }
            Session session = new Session(context.getApplicationContext(), generation, module, socket, dataSocket, token);
            sessions.put(generation, session);
            Intent intent = new Intent(context, WasmPluginService.class);
            intent.putExtra("generation", generation);
            try {
                session.bound = session.context.bindIsolatedService(intent, Context.BIND_AUTO_CREATE | Context.BIND_IMPORTANT,
                    "gen_" + generation, session.context.getMainExecutor(), session);
                if (!session.bound) { android.util.Log.w("PeersusModules", "Isolated binding was refused"); session.finish(); }
                main.postDelayed(() -> { if (session.uid < 0) { android.util.Log.w("PeersusModules", "Worker bootstrap did not identify itself"); session.stop(); } }, 5000);
            } catch (RuntimeException unavailable) { android.util.Log.w("PeersusModules", "Isolated binding failed: " + unavailable.getClass().getSimpleName()); session.finish(); }
        });
    }

    static void stop(long generation) {
        main.post(() -> { Session session = sessions.get(generation); if (session != null) session.stop(); });
    }

    private static final class Session implements ServiceConnection, IBinder.DeathRecipient {
        final Context context;
        final long generation;
        final String module, socket, dataSocket;
        String token;
        final AtomicBoolean live = new AtomicBoolean(true);
        final AtomicBoolean opening = new AtomicBoolean(false);
        final Messenger inbox;
        Messenger remote;
        IBinder binder;
        int uid = -1;
        boolean bound, linked, stopping;
        Session(Context context, long generation, String module, String socket, String dataSocket, String token) {
            this.context = context; this.generation = generation; this.module = module;
            this.socket = socket; this.dataSocket = dataSocket; this.token = token;
            inbox = new Messenger(new Handler(Looper.getMainLooper(), this::receive));
        }
        @Override public void onServiceConnected(ComponentName component, IBinder service) {
            if (!live.get()) return;
            binder = service;
            remote = new Messenger(service);
            try { binder.linkToDeath(this, 0); linked = true; }
            catch (RemoteException dead) { finish(); return; }
            if (stopping) { stop(); return; }
            io.execute(() -> {
                ParcelFileDescriptor moduleFd = null, control = null;
                try {
                    if (!live.get()) return;
                    File program = new File(module);
                    if (program.length() > 8 * 1024 * 1024) throw new IOException("module exceeds size bound");
                    moduleFd = ParcelFileDescriptor.open(program, ParcelFileDescriptor.MODE_READ_ONLY);
                    control = connect(socket, SocketKind.CONTROL);
                    ParcelFileDescriptor ownedModule = moduleFd, ownedControl = control;
                    moduleFd = control = null;
                    main.post(() -> {
                        try {
                            if (!live.get() || stopping) return;
                            Message attach = Message.obtain(null, ATTACH);
                            attach.replyTo = inbox;
                            Bundle values = attach.getData();
                            values.putLong("generation", generation);
                            values.putParcelable("module", ownedModule);
                            values.putParcelable("control", ownedControl);
                            values.putString("token", token);
                            remote.send(attach);
                            token = null;
                        } catch (RemoteException | RuntimeException unavailable) { stop(); }
                        finally { close(ownedModule); close(ownedControl); }
                    });
                } catch (IOException | RuntimeException unavailable) { main.post(this::stop); }
                finally { close(moduleFd); close(control); }
            });
        }
        private boolean receive(Message message) {
            if (!live.get() || stopping || remote == null || message.getData().getLong("generation", 0) != generation) return true;
            if (message.what == IDENTITY) {
                // Binder stamps sendingUid. Never trust a PID or UID from plugin-controlled JSON.
                if (uid >= 0 || message.sendingUid == Process.myUid() || message.sendingUid != message.arg2
                        || message.arg1 <= 0 || message.arg1 == Process.myPid()) { stop(); return true; }
                uid = message.sendingUid;
                return true;
            }
            if (message.what != OPEN_DATA || uid < 0 || message.sendingUid != uid) return true;
            long request = message.getData().getLong("request", 0);
            IBinder permit = message.getData().getBinder("permit");
            SocketKind kind = message.getData().getBoolean("canvas") ? SocketKind.CANVAS : SocketKind.DATA;
            if (request <= 0 || permit == null || !opening.compareAndSet(false, true)) return true;
            io.execute(() -> {
                ParcelFileDescriptor descriptor = null;
                try {
                    if (!live.get() || !allowed(permit)) return;
                    descriptor = connect(dataSocket, kind);
                    if (!live.get() || !allowed(permit)) return;
                    ParcelFileDescriptor owned = descriptor;
                    descriptor = null;
                    main.post(() -> {
                        try {
                            if (!live.get() || stopping) return;
                            Message response = Message.obtain(null, DATA_RESULT);
                            response.getData().putLong("generation", generation);
                            response.getData().putLong("request", request);
                            response.getData().putParcelable("descriptor", owned);
                            remote.send(response);
                        } catch (RemoteException | RuntimeException unavailable) { stop(); }
                        finally { close(owned); }
                    });
                } catch (IOException | RuntimeException unavailable) { main.post(this::stop); }
                finally { close(descriptor); opening.set(false); }
            });
            return true;
        }
        void stop() {
            if (!live.get()) return;
            stopping = true;
            if (remote != null) try {
                Message stop = Message.obtain(null, STOP);
                stop.getData().putLong("generation", generation);
                remote.send(stop);
            } catch (RemoteException dead) { finish(); return; }
            // AMS drops an isolated process after its last binding. Keep the death link:
            // unbinding alone is not evidence that the worker has actually terminated.
            if (bound) {
                bound = false;
                try { context.unbindService(this); }
                catch (IllegalArgumentException unavailable) { android.util.Log.w("PeersusModules", "Isolated binding could not be released"); }
            }
            if (binder == null || !binder.isBinderAlive()) finish();
        }
        void finish() {
            if (!live.getAndSet(false)) return;
            sessions.remove(generation, this);
            token = null;
            if (binder != null && linked) {
                linked = false;
                try { binder.unlinkToDeath(this, 0); } catch (RuntimeException ignored) {}
            }
            if (bound) {
                bound = false;
                try { context.unbindService(this); }
                catch (IllegalArgumentException unavailable) { android.util.Log.w("PeersusModules", "Isolated binding could not be released"); }
            }
            PluginBridge.moduleExited(generation);
        }
        @Override public void onServiceDisconnected(ComponentName name) { finish(); }
        @Override public void onBindingDied(ComponentName name) { finish(); }
        @Override public void onNullBinding(ComponentName name) { android.util.Log.w("PeersusModules", "Worker rejected its bootstrap binding"); finish(); }
        @Override public void binderDied() { main.post(this::finish); }
    }

    private static ParcelFileDescriptor connect(String path, SocketKind kind) throws IOException {
        // Connect AND configure in the owner UID, before Binder transfer. The isolated
        // worker retains SO_PEERCRED but cannot setsockopt on an owner-created socket.
        long sendWait = switch (kind) {
            case CONTROL -> 5000;
            case DATA -> 30000;
            // A canvas frame must not stall the interpreter for the ordinary data timeout.
            case CANVAS -> 25;
        };
        ParcelFileDescriptor descriptor = ParcelFileDescriptor.adoptFd(nativeConnect(path));
        try {
            android.system.Os.setsockoptTimeval(descriptor.getFileDescriptor(), android.system.OsConstants.SOL_SOCKET,
                android.system.OsConstants.SO_SNDTIMEO, android.system.StructTimeval.fromMillis(sendWait));
            if (kind == SocketKind.DATA) android.system.Os.setsockoptTimeval(descriptor.getFileDescriptor(), android.system.OsConstants.SOL_SOCKET,
                android.system.OsConstants.SO_RCVTIMEO, android.system.StructTimeval.fromMillis(30000));
            return descriptor;
        } catch (android.system.ErrnoException unavailable) {
            close(descriptor);
            throw new IOException("Plugin descriptor could not be configured");
        }
    }
    private static boolean allowed(IBinder permit) {
        Parcel input = Parcel.obtain(), output = Parcel.obtain();
        try { return permit.transact(IBinder.FIRST_CALL_TRANSACTION, input, output, 0) && output.readInt() == 1; }
        catch (RemoteException | RuntimeException unavailable) { return false; }
        finally { input.recycle(); output.recycle(); }
    }
    private static void close(ParcelFileDescriptor descriptor) {
        if (descriptor != null) try { descriptor.close(); } catch (IOException ignored) {}
    }
    private static native int nativeConnect(String path) throws IOException;
}
