package dev.peersus.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.Intent;
import android.os.Handler;
import android.os.Looper;
import android.widget.Toast;
import org.json.JSONArray;
import org.json.JSONObject;

/** Same-UID private Binder bridge. Tokens and clipboard contents never enter logs or storage. */
public final class PluginBridge {
    private static final java.security.SecureRandom random = new java.security.SecureRandom();
    private static final Handler main = new Handler(Looper.getMainLooper());
    private static PluginBridge instance;
    private final Context context;
    private String pendingShare;
    private String pendingText;
    private long pendingChoice, choiceEpoch, offerUntil, shareEpoch;

    private PluginBridge(Context context) { this.context = context.getApplicationContext(); }
    private static native void initializeTls(Context context);

    static void initialize(Context context) {
        initializeTls(context.getApplicationContext());
        ClipboardAccess.initialize(context);
        InputRecoveryProvider.initialize(context);
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

    public void stop(long generation) {
        ModuleBridge.stop(generation);
        NativeModules.stop(generation);
    }

    static void moduleExited(long generation) { nativeExited(generation); }

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
        instance.pendingChoice = 0;
        instance.pendingText = null;
        instance.pendingShare = value.toString();
        instance.offerUntil = android.os.SystemClock.elapsedRealtime() + 30000;
        if (activity.hasWindowFocus()) main.post(() -> focus(activity, true));
    }

    static void focus(Activity activity, boolean focused) {
        ClipboardAccess.focus(activity, focused);
        if (!focused || instance == null) return;
        if (instance.pendingChoice > 0) {
            long choice = instance.pendingChoice;
            String text = instance.pendingText;
            long epoch = instance.choiceEpoch;
            instance.pendingChoice = 0;
            instance.pendingText = null;
            if (epoch != instance.shareEpoch || text == null || !nativeShare(choice, text)) {
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
        if (instance != null) { instance.pendingShare = null; instance.pendingChoice = 0; instance.pendingText = null; instance.shareEpoch++; }
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
                        pendingChoice = selected.getLong("choice");
                        pendingText = text;
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

    private static native void nativeAttach(PluginBridge bridge);
    private static native void nativeExited(long generation);
    private static native String nativeTargets();
    private static native boolean nativeShare(long choice, String text);
}
