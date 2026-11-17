package dev.peersus.app;

import android.app.ActivityManager;
import android.content.*;
import android.os.*;
import java.util.HashMap;
import java.util.Map;

/** One declared service slot per live generation. Never release a running slot on a timer. */
final class NativeModules {
    private static final Handler main = new Handler(Looper.getMainLooper());
    private static final Map<Long, Session> sessions = new HashMap<>();
    private static final boolean[] slots = new boolean[NativeSlots.COUNT];
    static boolean launch(Context context, long generation, String module, String control, String data, String token) {
        if (generation <= 0 || module == null || control == null || data == null || token == null
                || !module.startsWith("/") || !control.startsWith("/") || !data.startsWith("/")
                || module.length() > 4096 || control.length() > 4096 || data.length() > 4096 || !token.matches("[0-9a-f]{64}")) return false;
        return main.post(() -> {
            if (sessions.containsKey(generation)) { PluginBridge.moduleExited(generation); return; }
            int slot = 0;
            while (slot < slots.length && slots[slot]) ++slot;
            if (slot == slots.length) { PluginBridge.moduleExited(generation); return; }
            Session session = new Session(context.getApplicationContext(), generation, slot, module, control, data, token);
            slots[slot] = true; sessions.put(generation, session);
            Intent intent = new Intent().setClassName(context, "dev.peersus.app.NativeSlots$Slot" + slot)
                .setAction("dev.peersus.native.GENERATION." + generation).putExtra("generation", generation);
            try {
                session.bound = session.context.bindService(intent, session, Context.BIND_AUTO_CREATE | Context.BIND_IMPORTANT);
                if (!session.bound) session.finish();
                else main.postDelayed(() -> { if (session.pid <= 0) session.stop(); }, 5000);
            } catch (RuntimeException unavailable) { session.finish(); }
        });
    }
    static void stop(long generation) { main.post(() -> { Session session = sessions.get(generation); if (session != null) session.stop(); }); }
    private static final class Session implements ServiceConnection, IBinder.DeathRecipient {
        final Context context;
        final long generation;
        final int slot;
        final String module, control, data;
        String token;
        final Messenger inbox;
        Messenger remote;
        IBinder binder;
        int pid;
        int exitCode = -1;
        boolean bound, linked, stopping, finished;
        Session(Context context, long generation, int slot, String module, String control, String data, String token) {
            this.context = context; this.generation = generation; this.slot = slot;
            this.module = module; this.control = control; this.data = data; this.token = token;
            inbox = new Messenger(new Handler(Looper.getMainLooper(), message -> {
                if (finished || message.sendingUid != android.os.Process.myUid() || message.getData().getLong("generation", 0) != generation) return true;
                if (message.what == ModuleBridge.EXIT) { exitCode = message.arg1; stop(); return true; }
                if (message.what == ModuleBridge.IDENTITY && message.arg2 == message.sendingUid
                        && message.arg1 > 0 && message.arg1 != android.os.Process.myPid()) {
                    pid = message.arg1;
                    if (stopping) forceStop();
                }
                return true;
            }));
        }
        @Override public void onServiceConnected(ComponentName name, IBinder service) {
            if (finished) return;
            binder = service; remote = new Messenger(service);
            try {
                binder.linkToDeath(this, 0); linked = true;
                if (stopping) { stop(); return; }
                Message attach = Message.obtain(null, ModuleBridge.ATTACH); attach.replyTo = inbox;
                Bundle values = attach.getData(); values.putLong("generation", generation);
                values.putString("module", module); values.putString("socket", control); values.putString("dataSocket", data); values.putString("token", token);
                remote.send(attach); token = null;
            } catch (RemoteException dead) { finish(); }
        }
        void stop() {
            if (finished) return;
            stopping = true; token = null;
            // A pending bind must reach its callback before the slot can be reused.
            if (remote == null) return;
            if (bound) {
                bound = false;
                try { context.unbindService(this); }
                catch (IllegalArgumentException unavailable) { android.util.Log.w("PeersusModules", "Native binding could not be released"); }
            }
            main.postDelayed(this::forceStop, 2000);
        }
        void forceStop() {
            if (finished) return;
            if (binder == null || !binder.isBinderAlive()) { finish(); return; }
            ActivityManager manager = context.getSystemService(ActivityManager.class);
            java.util.List<ActivityManager.RunningAppProcessInfo> running = manager == null ? null : manager.getRunningAppProcesses();
            if (running == null || pid <= 0) return;
            String expected = context.getPackageName() + ":native_" + slot;
            for (ActivityManager.RunningAppProcessInfo process : running) {
                if (process.pid == pid && process.uid == android.os.Process.myUid() && expected.equals(process.processName)) {
                    android.os.Process.killProcess(pid); return;
                }
            }
        }
        void finish() {
            if (finished) return;
            finished = true; token = null;
            sessions.remove(generation, this); slots[slot] = false;
            if (linked && binder != null) { linked = false; try { binder.unlinkToDeath(this, 0); } catch (RuntimeException ignored) {} }
            if (bound) {
                bound = false;
                try { context.unbindService(this); }
                catch (IllegalArgumentException unavailable) { android.util.Log.w("PeersusModules", "Native binding could not be released"); }
            }
            PluginBridge.moduleExited(generation);
        }
        @Override public void binderDied() { main.post(this::finish); }
        @Override public void onServiceDisconnected(ComponentName name) { finish(); }
        @Override public void onBindingDied(ComponentName name) { finish(); }
        @Override public void onNullBinding(ComponentName name) { finish(); }
    }
}
