package dev.peersus.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.Bundle;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Message;
import android.os.Parcel;
import android.os.Messenger;
import android.os.RemoteException;
import android.widget.Toast;
import java.util.HashMap;
import java.util.Map;
import org.json.JSONArray;
import org.json.JSONObject;

/** Same-UID private Binder bridge. Tokens and clipboard contents never enter logs or storage. */
public final class PluginBridge {
    static final int ATTACH = 1, STOP = 2, READ = 3, WRITE = 4, RESULT = 5, PID = 6, RESET = 7, SELECTED = 8, CHECK_SELECTED = 9;
    private static final java.security.SecureRandom random = new java.security.SecureRandom();
    private static final Handler main = new Handler(Looper.getMainLooper());
    private static PluginBridge instance;
    private final Context context;
    private final Map<Long, Session> sessions = new HashMap<>();
    private String pendingShare;
    private long pendingChoice, pendingContext, choiceEpoch, offerUntil, shareEpoch;

    private PluginBridge(Context context) { this.context = context.getApplicationContext(); }

    static void initialize(Context context) {
        ClipboardAccess.initialize(context);
        if (instance == null) { instance = new PluginBridge(context); nativeAttach(instance); }
    }

    static void attach(Activity activity) {
        initialize(activity);
        ClipboardAccess.attach(activity);
    }

    public void shareFailed() {
        main.post(() -> Toast.makeText(context, "Sharing was not accepted. Check device permissions.", Toast.LENGTH_LONG).show());
    }

    public boolean launchWasm(long generation, String module, String socket, String dataSocket, String token) {
        return ModuleBridge.launch(context, generation, module, socket, dataSocket, token);
    }

    public boolean launchNative(long generation, String module, String socket, String dataSocket, String token) {
        return NativeModules.launch(context, generation, module, socket, dataSocket, token);
    }

    static void moduleExited(long generation) { nativeExited(generation); }

    // Called by Rust from an attached worker. Only the main Handler touches sessions.
    public boolean launch(long generation, String socket, String dataSocket, String token) {
        if (generation <= 0 || socket == null || dataSocket == null || token == null
                || socket.length() > 4096 || dataSocket.length() > 4096 || token.length() > 256) return false;
        return main.post(() -> {
            if (!sessions.isEmpty()) {
                for (Session old : sessions.values().toArray(new Session[0])) old.stop();
                nativeExited(generation);
                return;
            }
            Session session = new Session(generation);
            sessions.put(generation, session);
            Intent launch = new Intent(context, PluginService.class);
            launch.setAction("dev.peersus.plugin.GENERATION." + generation);
            launch.putExtra("generation", generation);
            launch.putExtra("entry", "clipboard");
            launch.putExtra("socket", socket);
            launch.putExtra("dataSocket", dataSocket);
            launch.putExtra("token", token);
            try {
                session.bound = context.bindService(launch, session, Context.BIND_AUTO_CREATE | Context.BIND_IMPORTANT);
                if (!session.bound) session.finish();
                main.postDelayed(() -> { if (session.remote == null) session.finish(); }, 5000);
            } catch (RuntimeException unavailable) { session.finish(); }
        });
    }

    public void stop(long generation) {
        ModuleBridge.stop(generation);
        NativeModules.stop(generation);
        main.post(() -> {
            Session session = sessions.get(generation);
            if (session != null) session.stop();
        });
    }

    private final class Session implements ServiceConnection, IBinder.DeathRecipient {
        final long generation;
        final Messenger inbox;
        Messenger remote;
        IBinder binder;
        int pid;
        boolean bound, linked, stopping, finished;
        Session(long generation) {
            this.generation = generation;
            inbox = new Messenger(new Handler(Looper.getMainLooper(), this::receive));
        }
        private boolean receive(Message message) {
            if (finished || remote == null || message.sendingUid != android.os.Process.myUid()) return true;
            if (message.getData().getLong("generation", 0) != generation) return true;
            if (message.what == PID) {
                int reported = message.arg1;
                if (reported > 0 && reported != android.os.Process.myPid()) pid = reported;
                if (stopping) forceStop();
                return true;
            }
            if (stopping || (message.what != READ && message.what != WRITE && message.what != RESET && message.what != SELECTED && message.what != CHECK_SELECTED)) return true;
            Bundle request = message.getData();
            long id = request.getLong("request", 0);
            if (id <= 0 || request.getLong("deadline", 0) <= android.os.SystemClock.elapsedRealtime()) return true;
            // The capability is owned by the waiting operation, not by the queued Message.
            // Cancellation is serviced on a Binder thread, even while this Handler is blocked.
            IBinder permit = request.getBinder("permit");
            if (!allowed(permit)) return true;
            Message response = Message.obtain(null, RESULT);
            Bundle result = new Bundle();
            result.putLong("request", id);
            result.putLong("generation", generation);
            if (message.what == READ) {
                boolean observe = request.getBoolean("observe", false);
                result.putString("text", ClipboardAccess.read(!observe));
                result.putBoolean("changed", observe && ClipboardAccess.takeChanged());
            } else if (message.what == SELECTED) {
                result.putString("text", ClipboardAccess.readSelected(request.getLong("context", 0)));
            } else if (message.what == CHECK_SELECTED) {
                result.putBoolean("ok", ClipboardAccess.selectedValid(request.getLong("context", 0)));
            } else if (message.what == RESET) {
                ClipboardAccess.stopAutomatic();
                result.putBoolean("ok", true);
            } else {
                String text = request.getString("text");
                // Authorization is the write's linearization point; no queue turn follows it.
                result.putBoolean("ok", ClipboardAccess.write(text, () -> allowed(permit)
                    && request.getLong("deadline", 0) > android.os.SystemClock.elapsedRealtime()));
            }
            response.setData(result);
            try { remote.send(response); } catch (RemoteException dead) { finish(); }
            return true;
        }
        @Override public void onServiceConnected(ComponentName name, IBinder service) {
            if (finished) return;
            binder = service;
            remote = new Messenger(service);
            try {
                binder.linkToDeath(this, 0);
                linked = true;
                if (stopping) { stop(); return; }
                Message attach = Message.obtain(null, ATTACH);
                attach.replyTo = inbox;
                attach.getData().putLong("generation", generation);
                remote.send(attach);
            } catch (RemoteException dead) { finish(); }
        }
        @Override public void onServiceDisconnected(ComponentName name) { finish(); }
        @Override public void onBindingDied(ComponentName name) { finish(); }
        @Override public void onNullBinding(ComponentName name) { finish(); }
        @Override public void binderDied() { main.post(this::finish); }
        void stop() {
            stopping = true;
            if (remote == null) { finish(); return; }
            try {
                Message stop = Message.obtain(null, STOP);
                stop.getData().putLong("generation", generation);
                remote.send(stop);
            }
            catch (RemoteException dead) { finish(); }
            main.postDelayed(this::forceStop, 2000);
        }
        void forceStop() {
            if (finished) return;
            if (binder == null || !binder.isBinderAlive()) { finish(); return; }
            // PID comes only from the bound same-UID Service, never from an intent or plugin text.
            if (pid > 0 && pid != android.os.Process.myPid()) android.os.Process.killProcess(pid);
            // Completion is signalled by Binder death, not by assuming killProcess succeeded.
        }
        void finish() {
            if (finished) return;
            finished = true;
            sessions.remove(generation);
            if (binder != null && linked) {
                linked = false;
                try { binder.unlinkToDeath(this, 0); } catch (RuntimeException ignored) {}
            }
            if (bound) {
                bound = false;
                try { context.unbindService(this); } catch (RuntimeException ignored) {}
            }
            ClipboardAccess.stopAutomatic();
            ClipboardAccess.cancelShared();
            nativeExited(generation);
        }
    }

    static void offerSharedText(Activity activity, Intent intent) {
        if (instance == null || intent == null || !Intent.ACTION_SEND.equals(intent.getAction())
                || !"text/plain".equals(intent.getType())) return;
        CharSequence value;
        try { value = intent.getCharSequenceExtra(Intent.EXTRA_TEXT); }
        catch (RuntimeException malformed) { return; }
        intent.removeExtra(Intent.EXTRA_TEXT);
        intent.setAction(Intent.ACTION_MAIN);
        if (!ClipboardAccess.bounded(value)) {
            Toast.makeText(activity, "Shared text exceeds the Android clipboard limit (64 KiB).", Toast.LENGTH_LONG).show();
            return;
        }
        instance.shareEpoch++;
        ClipboardAccess.cancelShared();
        instance.pendingChoice = instance.pendingContext = 0;
        instance.pendingShare = value.toString();
        instance.offerUntil = android.os.SystemClock.elapsedRealtime() + 30000;
        if (activity.hasWindowFocus()) main.post(() -> focus(activity, true));
    }

    static void focus(Activity activity, boolean focused) {
        ClipboardAccess.focus(activity, focused);
        if (!focused || instance == null) return;
        if (instance.pendingChoice > 0) {
            long choice = instance.pendingChoice;
            long context = instance.pendingContext;
            long epoch = instance.choiceEpoch;
            instance.pendingChoice = instance.pendingContext = 0;
            if (epoch != instance.shareEpoch || !nativeShare(choice, context)) {
                ClipboardAccess.cancelShared();
                Toast.makeText(activity, "Sharing could not start. Check device permissions.", Toast.LENGTH_LONG).show();
            }
        }
        if (instance.pendingShare == null) return;
        String text = instance.pendingShare;
        instance.pendingShare = null;
        if (android.os.SystemClock.elapsedRealtime() > instance.offerUntil) return;
        long epoch = instance.shareEpoch;
        long deadline = Math.min(instance.offerUntil, android.os.SystemClock.elapsedRealtime() + 5000);
        main.post(() -> instance.chooseDestination(activity, text, deadline, epoch));
    }

    static void pause(Activity activity) {
        ClipboardAccess.pause(activity);
        ClipboardAccess.cancelShared();
        if (instance != null) { instance.pendingShare = null; instance.pendingChoice = instance.pendingContext = 0; instance.shareEpoch++; }
    }

    private void chooseDestination(Activity activity, String text, long deadline, long epoch) {
        if (epoch != shareEpoch || android.os.SystemClock.elapsedRealtime() >= deadline
                || activity.isFinishing() || !activity.hasWindowFocus()) return;
        try {
            String encoded = nativeTargets();
            JSONArray choices = encoded == null ? new JSONArray() : new JSONArray(encoded);
            if (choices.length() == 0 && android.os.SystemClock.elapsedRealtime() < deadline) {
                main.postDelayed(() -> chooseDestination(activity, text, deadline, epoch), 150);
                return;
            }
            if (choices.length() == 0) {
                Toast.makeText(activity, "No online device has clipboard and send permissions. Pair a device and grant both first.", Toast.LENGTH_LONG).show();
                return;
            }
            String[] names = new String[choices.length()];
            for (int i = 0; i < names.length; ++i) names[i] = choices.getJSONObject(i).getString("name");
            new AlertDialog.Builder(activity)
                .setTitle("Send shared text to…")
                .setItems(names, (dialog, index) -> {
                    if (epoch != shareEpoch || android.os.SystemClock.elapsedRealtime() >= offerUntil) return;
                    try {
                        JSONObject selected = choices.getJSONObject(index);
                        ++shareEpoch;
                        long context;
                        do { context = random.nextLong() & Long.MAX_VALUE; } while (context == 0);
                        ClipboardAccess.selectShared(context, text, android.os.SystemClock.elapsedRealtime() + 3000);
                        pendingChoice = selected.getLong("choice");
                        pendingContext = context;
                        choiceEpoch = shareEpoch;
                        main.post(() -> { if (activity.hasWindowFocus()) focus(activity, true); });
                    } catch (Exception unavailable) {
                        Toast.makeText(activity, "Sharing could not start.", Toast.LENGTH_LONG).show();
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
        } catch (Exception unavailable) {
            Toast.makeText(activity, "Clipboard sharing is not ready yet.", Toast.LENGTH_LONG).show();
        }
    }

    private static boolean allowed(IBinder permit) {
        if (permit == null) return false;
        Parcel data = Parcel.obtain(), reply = Parcel.obtain();
        try {
            return permit.transact(IBinder.FIRST_CALL_TRANSACTION, data, reply, 0)
                && reply.readInt() == 1;
        } catch (RemoteException | RuntimeException unavailable) { return false; }
        finally { data.recycle(); reply.recycle(); }
    }

    private static native void nativeAttach(PluginBridge bridge);
    private static native void nativeExited(long generation);
    private static native String nativeTargets();
    private static native boolean nativeShare(long choice, long context);
}
