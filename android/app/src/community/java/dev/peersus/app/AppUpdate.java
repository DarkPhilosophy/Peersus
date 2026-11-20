package dev.peersus.app;

import android.app.Activity;
import android.content.ComponentName;
import android.content.Context;
import android.content.ServiceConnection;
import android.content.pm.PackageManager;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.RemoteException;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.TimeUnit;
import rikka.shizuku.Shizuku;

/**
 * This distribution replaces itself: an update the owner verified is handed to Android through an already-running shell-mode
 * Stellar/Shizuku provider, with no question asked by Android. Without that provider nothing is installed and the owner says why.
 */
final class AppUpdate {
    private static final int REQUEST = 7243;
    /** Results beyond the worker's own (3, 4, 5): 1 no shell-mode provider is running, 2 the provider has not allowed this application. */
    private static final int NO_PROVIDER = 1, NOT_ALLOWED = 2, MISUSED = 3, REFUSED = 4, LATE = 5;

    /** Whether something else than the application keeps this installation up to date. */
    static boolean managed() { return false; }
    static void resumed(Activity owner) { }

    /**
     * Blocking, on the owner's blocking pool and never on the main thread. A successful replacement ends this process before the
     * call returns, so every value that does return says why the update is not in place.
     */
    static int install(Context owner, String path, boolean reopen) {
        if (Looper.myLooper() == Looper.getMainLooper() || path == null) return MISUSED;
        Context context = owner.getApplicationContext();
        try {
            if (!Shizuku.pingBinder() || Shizuku.isPreV11() || Shizuku.getUid() != 2000) return NO_PROVIDER;
        } catch (RuntimeException unavailable) { return NO_PROVIDER; }
        if (!ShellProvider.granted()) {
            // The provider's own interactive approval is mandatory; the person then asks for the update again.
            try { Shizuku.requestPermission(REQUEST); } catch (RuntimeException unavailable) { return NO_PROVIDER; }
            return NOT_ALLOWED;
        }
        File file = new File(path);
        long size = file.length();
        if (size < 1 || size > AppUpdateUserService.MAX_BYTES) return MISUSED;
        long installed;
        try { installed = context.getPackageManager().getPackageInfo(context.getPackageName(), 0).lastUpdateTime; }
        catch (PackageManager.NameNotFoundException unavailable) { installed = 0; }
        // The provider may keep a worker alive across app updates; a per-install tag never reaches stale code.
        Shizuku.UserServiceArgs arguments = new Shizuku.UserServiceArgs(new ComponentName(context, AppUpdateUserService.class))
            .tag("app-update-" + installed).version(1).daemon(false).debuggable(false).processNameSuffix("app_update");
        // Holds the worker until it is taken: the provider may answer before this thread waits.
        ArrayBlockingQueue<IBinder> arrived = new ArrayBlockingQueue<>(1);
        ServiceConnection connection = new ServiceConnection() {
            @Override public void onServiceConnected(ComponentName name, IBinder binder) { arrived.offer(binder); }
            @Override public void onServiceDisconnected(ComponentName name) { }
        };
        Handler main = new Handler(Looper.getMainLooper());
        try {
            main.post(() -> {
                try { Shizuku.bindUserService(arguments, connection); }
                catch (RuntimeException unavailable) { /* The wait below ends without a worker. */ }
            });
            IBinder binder = arrived.poll(15, TimeUnit.SECONDS);
            if (binder == null) return NO_PROVIDER;
            IAppUpdate worker = IAppUpdate.Stub.asInterface(binder);
            int result = worker.begin(size, reopen);
            if (result != 0) return result;
            byte[] chunk = new byte[AppUpdateUserService.MAX_CHUNK];
            try (FileInputStream input = new FileInputStream(file)) {
                for (long sent = 0; sent < size; ) {
                    int read = input.read(chunk, 0, (int) Math.min(chunk.length, size - sent));
                    if (read < 1) return MISUSED;
                    result = worker.write(read == chunk.length ? chunk : java.util.Arrays.copyOf(chunk, read));
                    if (result != 0) return result;
                    sent += read;
                }
            }
            result = worker.finish();
            // Android said yes and this process still runs: it is about to be replaced. Not a failure to report.
            return result;
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return LATE;
        } catch (IOException | RemoteException | RuntimeException unavailable) {
            return REFUSED;
        } finally {
            main.post(() -> {
                try { Shizuku.unbindUserService(arguments, connection, true); }
                catch (RuntimeException unavailable) { /* Provider death already ends its worker. */ }
            });
        }
    }
    private AppUpdate() { }
}
