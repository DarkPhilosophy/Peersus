package dev.peersus.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.content.pm.PackageManager;
import android.database.ContentObserver;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.SystemClock;
import android.provider.Settings;
import android.widget.Toast;
import rikka.shizuku.Shizuku;

/**
 * The remote input switch: on turns this app's own accessibility service on through an already-running shell-mode Stellar/Shizuku
 * provider and keeps it connected; off turns it off. Only this app's service is ever touched. Turning it off in Android's own settings
 * while the app runs is the person's choice and turns the switch off; it is never undone.
 */
final class InputRecoveryProvider {
    private static final Handler main = new Handler(Looper.getMainLooper());
    private static final int REQUEST = 7242;
    private static final int RECONNECT = 0, ENABLE = 1, DISABLE = 2;
    private static Context context;
    private static volatile boolean enabled;
    private static volatile boolean wanted;
    private static volatile long generation;
    /** Our own write is in flight: a change of the list seen now is ours, not the person's. */
    private static boolean busy;
    private static int pending = -1;
    private static long attemptedAt = -30000;
    private static Shizuku.UserServiceArgs arguments;
    private static ServiceConnection connection;
    private static volatile String status = "Off";
    private static final java.util.concurrent.ExecutorService worker =
        java.util.concurrent.Executors.newSingleThreadExecutor(task -> {
            Thread thread = new Thread(task, "input-recovery");
            thread.setDaemon(true);
            return thread;
        });

    static String status() {
        String connection = RemoteInput.connected() ? "Connected." : "Not connected.";
        if (!enabled) return "Off. " + connection + " Turn on to let paired devices you allowed control this phone.";
        String note = wanted ? "" : " No device is allowed to control this phone yet.";
        return status + " " + connection + note;
    }
    static boolean on() { return enabled; }

    /** The switch: the person's flip is the consent. On may first show the provider's own permission prompt. */
    static void set(Activity owner, boolean value) {
        if (context == null) initialize(owner);
        main.post(() -> {
            if (!value) {
                save(false);
                status = "Off";
                run(DISABLE);
                return;
            }
            if (!Shizuku.pingBinder()) {
                toast(owner, "Start Stellar/Shizuku (with Shizuku compatibility) first.");
                return;
            }
            try {
                if (Shizuku.isPreV11() || Shizuku.getUid() != 2000) {
                    toast(owner, "A shell-mode provider is required; root mode is not used.");
                    return;
                }
            } catch (RuntimeException unavailable) { toast(owner, "Provider unavailable."); return; }
            save(true);
            attemptedAt = -30000;
            if (ShellProvider.granted()) run(ENABLE);
            else {
                status = "Waiting for the provider's permission…";
                try { Shizuku.requestPermission(REQUEST); }
                catch (RuntimeException unavailable) { save(false); status = "Provider unavailable."; }
            }
        });
    }
    static void wanted(Context owner, boolean value) {
        if (context == null) initialize(owner);
        wanted = value;
    }
    static void resumed(Activity owner) { request(); }

    static void initialize(Context owner) {
        Context application = owner.getApplicationContext();
        main.post(() -> {
            if (context != null) return;
            context = application;
            enabled = context.getSharedPreferences("input-recovery", 0).getBoolean("enabled", false);
            long installed;
            try { installed = context.getPackageManager().getPackageInfo(context.getPackageName(), 0).lastUpdateTime; }
            catch (PackageManager.NameNotFoundException unavailable) { installed = 0; }
            // The provider may keep a worker alive across app updates; a per-install tag never reaches stale code.
            arguments = new Shizuku.UserServiceArgs(new ComponentName(context, InputRecoveryUserService.class))
                .tag("input-recovery-" + installed).version(4).daemon(false).debuggable(false).processNameSuffix("input_recovery");
            context.getContentResolver().registerContentObserver(
                Settings.Secure.getUriFor(Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES), false,
                new ContentObserver(main) {
                    @Override public void onChange(boolean selfChange) {
                        // Turned off in Android's settings while the app runs: the person's choice, so the switch follows it.
                        if (enabled && !busy && !RemoteInput.approved(context)) { save(false); status = "Turned off in Android settings."; }
                    }
                });
            Shizuku.addBinderReceivedListenerSticky(InputRecoveryProvider::request);
            Shizuku.addBinderDeadListener(() -> main.post(() -> finish("Provider stopped. Start Stellar/Shizuku to keep it connected.")));
            Shizuku.addRequestPermissionResultListener((code, result) -> {
                if (code != REQUEST) return;
                main.post(() -> {
                    if (result != PackageManager.PERMISSION_GRANTED) { save(false); status = "Provider permission refused."; }
                    else run(ENABLE);
                });
            });
            request();
        });
    }

    /** Keep a switched-on service on and connected; event driven, at most one bounded repair at a time. */
    static void request() {
        main.post(() -> {
            if (context == null || !enabled || busy) return;
            if (!Shizuku.pingBinder()) { status = "Start Stellar/Shizuku to keep it connected."; return; }
            if (!ShellProvider.granted()) { status = "Provider permission was revoked."; return; }
            // Android removes the service of a stopped app; the switch is still on, so it is put back.
            if (!RemoteInput.approved(context)) { run(ENABLE); return; }
            if (RemoteInput.connected()) { status = "On."; return; }
            if (SystemClock.elapsedRealtime() - attemptedAt < 30000) return;
            attemptedAt = SystemClock.elapsedRealtime();
            run(RECONNECT);
        });
    }

    private static void run(int operation) {
        if (busy) { pending = operation; return; }
        if (!ShellProvider.granted()) { if (operation == DISABLE) status = "Off. Turn it off in Android settings too: the provider is unavailable."; return; }
        busy = true;
        status = operation == DISABLE ? "Turning off…" : operation == ENABLE ? "Turning on…" : "Reconnecting…";
        long epoch = ++generation;
        connection = new ServiceConnection() {
            @Override public void onServiceConnected(ComponentName name, IBinder binder) {
                if (epoch != generation) return;
                IInputRecovery recovery = IInputRecovery.Stub.asInterface(binder);
                worker.execute(() -> {
                    int result;
                    try {
                        IInputRecoveryConsent consent = new IInputRecoveryConsent.Stub() {
                            @Override public boolean allowed() {
                                return epoch == generation && (operation == DISABLE ? !enabled : enabled) && ShellProvider.granted();
                            }
                        };
                        result = operation == ENABLE ? recovery.enable(consent)
                            : operation == DISABLE ? recovery.disable(consent) : recovery.reconnect(consent);
                    } catch (android.os.RemoteException | RuntimeException unavailable) { result = 7; }
                    int outcome = result;
                    main.post(() -> {
                        if (epoch != generation) return;
                        if (operation == DISABLE) { finish(outcome == 0 ? "Off" : "Could not turn it off (" + outcome + ")."); return; }
                        if (outcome == 11) {
                            save(false);
                            finish("Android accessibility is off for other services too; turn it on once in Android settings.");
                            return;
                        }
                        if (outcome != 0) { finish("Could not turn it on (" + outcome + ")."); return; }
                        // A settings write is not evidence of a connected service.
                        main.postDelayed(() -> {
                            if (epoch == generation) finish(RemoteInput.connected() ? "On." : "Turned on; waiting for Android to connect it.");
                        }, 2500);
                    });
                });
            }
            @Override public void onServiceDisconnected(ComponentName name) {
                if (epoch == generation) finish("Provider disconnected.");
            }
        };
        try {
            Shizuku.bindUserService(arguments, connection);
            main.postDelayed(() -> { if (epoch == generation) finish("Timed out."); }, 15000);
        } catch (RuntimeException unavailable) { finish("Provider unavailable."); }
    }

    private static void finish(String message) {
        ++generation;
        busy = false;
        status = message;
        ServiceConnection previous = connection;
        connection = null;
        if (previous != null) try { Shizuku.unbindUserService(arguments, previous, true); }
        catch (RuntimeException unavailable) { /* Provider death already ends its isolated worker. */ }
        if (pending >= 0) { int next = pending; pending = -1; run(next); }
    }
    private static void save(boolean value) {
        enabled = value;
        context.getSharedPreferences("input-recovery", 0).edit().putBoolean("enabled", value).apply();
    }
    private static void toast(Activity owner, String message) {
        status = message;
        Toast.makeText(owner, message, Toast.LENGTH_LONG).show();
    }
    /** Details and the way to Android's own settings. */
    static void open(Activity owner) {
        if (context == null) initialize(owner);
        main.post(() -> new AlertDialog.Builder(owner).setTitle("Remote control of this phone")
            .setMessage(status() + "\n\nThe switch turns this app's accessibility service on or off through Stellar/Shizuku and keeps it connected after the app restarts. Other services are not changed. Turning it off in Android settings also turns the switch off.")
            .setNeutralButton("Android settings", (dialog, which) -> owner.startActivity(new Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)))
            .setPositiveButton("Close", null).show());
    }
    private InputRecoveryProvider() { }
}
