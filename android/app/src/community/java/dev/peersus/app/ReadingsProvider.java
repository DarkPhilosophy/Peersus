package dev.peersus.app;

import android.content.ComponentName;
import android.content.Context;
import android.content.ServiceConnection;
import android.content.pm.PackageManager;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.RemoteException;
import android.os.SystemClock;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;
import rikka.shizuku.Shizuku;

/**
 * What Android closes to an application, read through an already-running shell-mode Stellar/Shizuku provider the person already
 * allowed: how busy the processors are. Nothing is asked of anybody here; without that provider the reading stays unknown.
 *
 * The caller (the native owner) never crosses into the provider: a worker of its own reads the line once a second while it is
 * being asked for, and the caller is handed the last line the worker read, or nothing when that is older than three seconds. A
 * provider that hangs holds up only that worker.
 */
final class ReadingsProvider {
    private static final Handler main = new Handler(Looper.getMainLooper());
    private static final ScheduledExecutorService worker = Executors.newSingleThreadScheduledExecutor(task -> {
        Thread thread = new Thread(task, "readings");
        thread.setDaemon(true);
        return thread;
    });
    private static volatile IReadings readings;
    private static volatile String latest;
    private static volatile long readAt = Long.MIN_VALUE / 2;
    private static volatile long askedAt = Long.MIN_VALUE / 2;
    private static boolean binding;
    private static boolean reading;
    private static long attemptedAt = -30000;
    private static Shizuku.UserServiceArgs arguments;

    /** The first line of /proc/stat as last read (at most three seconds ago), or null. Never calls the provider. */
    static String processorTime(Context context) {
        long now = SystemClock.elapsedRealtime();
        askedAt = now;
        if (readings == null) bind(context.getApplicationContext());
        else main.post(ReadingsProvider::read);
        return now - readAt <= 3000 ? latest : null;
    }

    /** Keep the worker reading once a second for as long as the line is asked for (five seconds without a question ends it). */
    private static void read() {
        if (reading || readings == null) return;
        reading = true;
        worker.execute(new Runnable() {
            @Override public void run() {
                IReadings bound = readings;
                if (bound == null || SystemClock.elapsedRealtime() - askedAt > 5000) {
                    main.post(() -> reading = false);
                    return;
                }
                try {
                    String line = bound.processorTime();
                    latest = line;
                    readAt = SystemClock.elapsedRealtime();
                } catch (RemoteException | RuntimeException unavailable) {
                    readings = null;
                    main.post(() -> reading = false);
                    return;
                }
                worker.schedule(this, 1, TimeUnit.SECONDS);
            }
        });
    }

    private static void bind(Context application) {
        main.post(() -> {
            // At most one attempt every 30 seconds: a reading is asked every second while a page shows it.
            if (binding || readings != null || SystemClock.elapsedRealtime() - attemptedAt < 30000) return;
            attemptedAt = SystemClock.elapsedRealtime();
            if (!ShellProvider.granted()) return;
            if (arguments == null) {
                long installed;
                try { installed = application.getPackageManager().getPackageInfo(application.getPackageName(), 0).lastUpdateTime; }
                catch (PackageManager.NameNotFoundException unavailable) { installed = 0; }
                // A per-install tag never reaches a worker left from an older build.
                arguments = new Shizuku.UserServiceArgs(new ComponentName(application, ReadingsUserService.class))
                    .tag("readings-" + installed).version(1).daemon(false).debuggable(false).processNameSuffix("readings");
                Shizuku.addBinderDeadListener(() -> main.post(() -> { readings = null; binding = false; }));
            }
            binding = true;
            try {
                Shizuku.bindUserService(arguments, new ServiceConnection() {
                    @Override public void onServiceConnected(ComponentName name, IBinder binder) {
                        readings = IReadings.Stub.asInterface(binder);
                        binding = false;
                        read();
                    }
                    @Override public void onServiceDisconnected(ComponentName name) {
                        readings = null;
                        binding = false;
                    }
                });
            } catch (RuntimeException unavailable) { binding = false; }
        });
    }
    private ReadingsProvider() { }
}
