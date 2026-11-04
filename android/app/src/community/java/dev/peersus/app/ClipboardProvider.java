package dev.peersus.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ComponentName;
import android.content.Context;
import android.content.ServiceConnection;
import android.content.pm.PackageManager;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.RemoteException;
import rikka.shizuku.Shizuku;
import moe.shizuku.server.IShizukuService;

/** Main-thread-only optional source. Granting the provider never grants any peer permission. */
final class ClipboardProvider {
    private static final int REQUEST = 7241;
    private static final Handler main = new Handler(Looper.getMainLooper());
    private static Context context;
    private static boolean enabled, wanted, binding;
    private static long generation;
    private static IClipboardSource remote;
    private static Shizuku.UserServiceArgs arguments;
    private static String failure;
    private static ServiceConnection connection;
    private static volatile String publishedStatus = "Off. Ordinary clipboard sending requires focus.";
    private static final java.util.concurrent.atomic.AtomicBoolean changeQueued = new java.util.concurrent.atomic.AtomicBoolean();

    static void initialize(Context owner) {
        if (context != null) return;
        context = owner.getApplicationContext();
        enabled = context.getSharedPreferences("clipboard-provider", 0).getBoolean("enabled", false);
        arguments = new Shizuku.UserServiceArgs(new ComponentName(context, ClipboardUserService.class))
            .tag("clipboard-source-v1").version(1).daemon(false).debuggable(false).processNameSuffix("clipboard_source");
        Shizuku.addBinderReceivedListenerSticky(() -> { failure = null; refresh(); });
        Shizuku.addBinderDeadListener(() -> { disconnect(); ClipboardAccess.providerChanged(true); });
        Shizuku.addRequestPermissionResultListener((code, result) -> {
            if (code != REQUEST) return;
            if (result != PackageManager.PERMISSION_GRANTED) setEnabled(false);
            else { failure = null; refresh(); }
        });
        updateStatus();
    }

    static boolean supported() { return true; }

    private static boolean granted() {
        try {
            // checkSelfPermission() in the SDK caches a grant. Query the provider itself so revocation is immediate.
            return Shizuku.pingBinder() && !Shizuku.isPreV11() && Shizuku.getUid() == 2000
                && IShizukuService.Stub.asInterface(Shizuku.getBinder()).checkSelfPermission();
        } catch (RemoteException | RuntimeException unavailable) { return false; }
    }

    static boolean active() {
        if (!enabled || remote == null) return false;
        if (!granted()) { disconnect(); ClipboardAccess.providerChanged(true); return false; }
        if (!remote.asBinder().isBinderAlive()) { disconnect(); ClipboardAccess.providerChanged(true); return false; }
        return true;
    }

    static String status() {
        main.post(ClipboardProvider::updateStatus);
        return publishedStatus;
    }

    private static void updateStatus() { publishedStatus = describe(); }

    private static String describe() {
        if (context == null) return "Off";
        if (!Shizuku.pingBinder()) return "Provider stopped or unavailable. Start Stellar/Shizuku and enable its compatibility mode.";
        try {
            if (Shizuku.getUid() != 2000) return "A shell-mode provider is required; root mode is not used.";
        } catch (RuntimeException unavailable) { return "Provider unavailable"; }
        if (!enabled) return "Off. Ordinary clipboard sending requires focus.";
        if (!granted()) return "Provider permission required or revoked. Ordinary clipboard sending requires focus.";
        if (failure != null) return failure;
        if (active()) return "On. New non-sensitive text can be sent while the app is unfocused, subject to your per-device permissions.";
        return binding ? "Connecting to provider…" : "Allowed. Waiting for automatic clipboard synchronization to start.";
    }

    static void open(Activity owner) {
        if (enabled) {
            AlertDialog.Builder dialog = new AlertDialog.Builder(owner).setTitle("Background clipboard access")
                .setMessage(describe()).setPositiveButton("Turn off", (view, which) -> setEnabled(false))
                .setNegativeButton("Close", null);
            if (!active()) dialog.setNeutralButton("Retry", (view, which) -> {
                failure = null;
                disconnect();
                if (granted()) refresh();
                else if (Shizuku.pingBinder()) try { Shizuku.requestPermission(REQUEST); }
                catch (RuntimeException unavailable) { updateStatus(); }
            });
            dialog.show();
            return;
        }
        new AlertDialog.Builder(owner).setTitle("Optional background clipboard access")
            .setMessage("This lets the app read new clipboard text from other apps using an already-running shell-mode Stellar/Shizuku provider. Only enable it on a device you trust. Sensitive, non-text and oversized clips are excluded. Existing per-device send permissions are still required. You can turn this off here or revoke access in the provider. No root, app-op or system permission is changed automatically.")
            .setNegativeButton("Cancel", null)
            .setPositiveButton("Continue", (dialog, which) -> {
                if (!Shizuku.pingBinder()) {
                    new AlertDialog.Builder(owner).setMessage("Start Stellar/Shizuku first. For Stellar, enable Shizuku compatibility in its own settings.")
                        .setPositiveButton("Close", null).show();
                    return;
                }
                try {
                    if (Shizuku.isPreV11() || Shizuku.getUid() != 2000) return;
                    setEnabled(true);
                    if (granted()) refresh();
                    else Shizuku.requestPermission(REQUEST); // The provider's own interactive approval is mandatory.
                } catch (RuntimeException unavailable) { setEnabled(false); }
            }).show();
    }

    private static void setEnabled(boolean value) {
        enabled = value;
        failure = null;
        context.getSharedPreferences("clipboard-provider", 0).edit().putBoolean("enabled", value).apply();
        disconnect();
        ClipboardAccess.providerChanged(true);
        updateStatus();
        if (value) refresh();
    }

    static void observe(boolean value) {
        wanted = value;
        if (!value) disconnect();
        else refresh();
    }

    private static void refresh() {
        if (!enabled || !wanted || binding || remote != null || failure != null || !granted()) return;
        binding = true;
        long epoch = ++generation;
        connection = new ServiceConnection() {
            @Override public void onServiceConnected(ComponentName name, IBinder binder) {
                if (epoch != generation || !enabled || !wanted || !granted()) return;
                binding = false;
                IClipboardSource source = IClipboardSource.Stub.asInterface(binder);
                try {
                    if (!source.start(android.os.Process.myUid() / 100000, new IClipboardChange.Stub() {
                        @Override public void changed() {
                            if (!changeQueued.compareAndSet(false, true)) return;
                            main.post(() -> {
                                changeQueued.set(false);
                                if (epoch == generation && active()) ClipboardAccess.providerChanged(false);
                            });
                        }
                    })) throw new IllegalStateException();
                    remote = source;
                    // A new subscription is a new source boundary: never replay what preceded it.
                    ClipboardAccess.providerChanged(true);
                } catch (RemoteException | RuntimeException unavailable) {
                    failure = "Provider clipboard source unavailable. Ordinary clipboard sending requires focus.";
                    disconnect();
                }
            }
            @Override public void onServiceDisconnected(ComponentName name) {
                if (epoch != generation) return;
                disconnect();
                ClipboardAccess.providerChanged(true);
            }
        };
        try {
            Shizuku.bindUserService(arguments, connection);
            main.postDelayed(() -> {
                if (epoch == generation && binding) {
                    failure = "Provider connection timed out. Turn off and retry after starting the provider.";
                    disconnect();
                }
            }, 5000);
        } catch (RuntimeException unavailable) {
            failure = "Provider unavailable or compatibility disabled. Ordinary clipboard sending requires focus.";
            disconnect();
        }
    }

    static ClipData read() {
        if (!active()) return null;
        try { return remote.read(); }
        catch (RemoteException | RuntimeException unavailable) {
            disconnect();
            ClipboardAccess.providerChanged(true);
            return null;
        }
    }

    private static void disconnect() {
        ++generation;
        IClipboardSource source = remote;
        remote = null;
        boolean attached = binding || connection != null;
        binding = false;
        if (source != null) try { source.stop(); } catch (RemoteException | RuntimeException ignored) {}
        if (attached && arguments != null) try { Shizuku.unbindUserService(arguments, connection, true); }
        catch (RuntimeException ignored) {}
        connection = null;
    }
}
