package dev.peersus.app;

import android.app.Service;
import android.content.Intent;
import android.os.*;

/** External distribution only. Separate process for crashes, NOT an application-UID sandbox. */
public class NativePluginService extends Service {
    static { System.loadLibrary("peersus_native_android"); }
    private final Messenger inbox = new Messenger(new Handler(Looper.getMainLooper(), this::receive));
    private long generation;
    private boolean started;
    private IBinder owner;
    private final IBinder.DeathRecipient death = NativePluginService::terminate;

    @Override public IBinder onBind(Intent intent) {
        String process = android.app.Application.getProcessName();
        if (intent == null || started || android.os.Process.isIsolated() || !process.matches(".*:native_[0-9]+")) return null;
        // Ordinary services may be rebound with a framework-retained Intent. The
        // authenticated first ATTACH, not that Intent's extras, commits a generation.
        return inbox.getBinder();
    }
    private boolean receive(Message message) {
        if (message.sendingUid != getApplicationInfo().uid) return true;
        if (started && message.getData().getLong("generation", 0) != generation || message.what == ModuleBridge.ATTACH && started) {
            if (message.what == ModuleBridge.ATTACH && message.replyTo != null) try {
                Message refusal = Message.obtain(null, ModuleBridge.EXIT); refusal.arg1 = 1002;
                refusal.getData().putLong("generation", message.getData().getLong("generation", 0)); message.replyTo.send(refusal);
            } catch (RemoteException ignored) {}
            return true;
        }
        if (message.what == ModuleBridge.STOP) { stopSelf(); return true; }
        if (message.what != ModuleBridge.ATTACH || started || message.replyTo == null) return true;
        Bundle values = message.getData();
        long requestedGeneration = values.getLong("generation", 0);
        String module = values.getString("module"), control = values.getString("socket"), data = values.getString("dataSocket"), token = values.getString("token");
        if (requestedGeneration <= 0 || !absolute(module) || !absolute(control) || !absolute(data) || token == null || !token.matches("[0-9a-f]{64}")) { terminate(); return true; }
        generation = requestedGeneration;
        started = true;
        owner = message.replyTo.getBinder();
        try {
            owner.linkToDeath(death, 0);
            Message identity = Message.obtain(null, ModuleBridge.IDENTITY);
            identity.arg1 = android.os.Process.myPid(); identity.arg2 = android.os.Process.myUid();
            identity.getData().putLong("generation", generation);
            message.replyTo.send(identity);
        } catch (RemoteException dead) { terminate(); return true; }
        Messenger reply = message.replyTo;
        new Thread(() -> {
            int result = 1;
            try { result = nativeRun(module, control, data, token); }
            finally {
                try {
                    Message exit = Message.obtain(null, ModuleBridge.EXIT); exit.arg1 = result;
                    exit.getData().putLong("generation", generation); reply.send(exit);
                    // The owner unbinds first; destruction finishes its framework callback
                    // before the queued process exit, avoiding a crashed bound-service restart.
                } catch (RemoteException dead) { terminate(); }
            }
        }, "native-extension").start();
        return true;
    }
    private static boolean absolute(String path) { return path != null && path.startsWith("/") && path.length() <= 4096; }
    private static void terminate() {
        if (!android.os.Process.isIsolated() && android.app.Application.getProcessName().matches(".*:native_[0-9]+")) android.os.Process.killProcess(android.os.Process.myPid());
    }
    @Override public boolean onUnbind(Intent intent) { return false; }
    @Override public void onDestroy() {
        super.onDestroy();
        new Handler(Looper.getMainLooper()).post(NativePluginService::terminate);
    }
    private static native int nativeRun(String module, String control, String data, String token);
}
