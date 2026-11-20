package dev.peersus.app;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationManager;
import android.os.CancellationSignal;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executors;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Where this phone is now, for an extension the person allowed to know it. Called from Rust, never on the main thread: it may wait a
 * few seconds for the system's first fix. Nothing is kept.
 */
final class DeviceLocation {
    /** Runtime permission result routed back by the activity to the original lookup. */
    static final int PERMISSION = 7005;
    /** The answer when the person's setting refuses, told apart from no fix. */
    static final String REFUSED = "refused";
    /** A last known fix younger than this is answered as it is, without waking a provider. */
    private static final long FRESH_MS = 2 * 60 * 1000;
    /** How long a fresh fix is waited for: under the application's 20-second ceiling for the call. */
    private static final long WAIT_MS = 15 * 1000;
    private static final java.util.WeakHashMap<Activity, CountDownLatch> prompts = new java.util.WeakHashMap<>();

    static synchronized void permissionResult(Activity activity) {
        CountDownLatch pending = prompts.remove(activity);
        if (pending != null) pending.countDown();
    }

    private static boolean allowed(Context context) {
        return context.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
                || context.checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED;
    }

    private static synchronized CountDownLatch prompt(Activity activity) {
        CountDownLatch existing = prompts.get(activity);
        if (existing != null) return existing;
        CountDownLatch pending = new CountDownLatch(1);
        prompts.put(activity, pending);
        activity.runOnUiThread(() -> {
            if (activity.isFinishing() || activity.isDestroyed() || allowed(activity)) {
                permissionResult(activity);
            } else {
                activity.requestPermissions(new String[] {Manifest.permission.ACCESS_FINE_LOCATION,
                    Manifest.permission.ACCESS_COARSE_LOCATION}, PERMISSION);
            }
        });
        return pending;
    }

    private DeviceLocation() {}

    /**
     * {@code "latitude longitude accuracy time"} (accuracy in metres or {@code -}, time in milliseconds since 1970); {@link #REFUSED}
     * when the person's setting refuses (no permission, then the system asks the person when the window is open; or location turned
     * off); null when no fix came in time.
     */
    static String find(Context context) {
        long deadline = android.os.SystemClock.elapsedRealtime() + WAIT_MS;
        if (!allowed(context)) {
            if (!(context instanceof Activity)) return REFUSED;
            try {
                if (!prompt((Activity)context).await(WAIT_MS, TimeUnit.MILLISECONDS)) return "permission-pending";
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                return null;
            }
            if (!allowed(context)) return REFUSED;
        }
        LocationManager manager = context.getSystemService(LocationManager.class);
        if (manager == null) return null;
        if (!manager.isLocationEnabled()) return REFUSED;
        try {
            Location best = null;
            for (String provider : manager.getProviders(true)) {
                Location known;
                try { known = manager.getLastKnownLocation(provider); }
                catch (SecurityException unavailableProvider) { continue; }
                if (known != null && (best == null || known.getElapsedRealtimeNanos() > best.getElapsedRealtimeNanos())) best = known;
            }
            if (best == null || System.currentTimeMillis() - best.getTime() > FRESH_MS) {
                Location fresh = current(manager, Math.max(1, deadline - android.os.SystemClock.elapsedRealtime()));
                if (fresh != null) best = fresh;
            }
            if (best == null) return null;
            return best.getLatitude() + " " + best.getLongitude() + " "
                    + (best.hasAccuracy() ? Float.toString(best.getAccuracy()) : "-") + " " + best.getTime();
        } catch (SecurityException refused) {
            // Taken back between the check and the read.
            return REFUSED;
        }
    }

    /** One fresh fix from the best enabled provider, or null when none came in time. */
    private static Location current(LocationManager manager, long remainingMs) {
        String provider = manager.isProviderEnabled(LocationManager.FUSED_PROVIDER) ? LocationManager.FUSED_PROVIDER
                : manager.isProviderEnabled(LocationManager.NETWORK_PROVIDER) ? LocationManager.NETWORK_PROVIDER
                : manager.isProviderEnabled(LocationManager.GPS_PROVIDER) ? LocationManager.GPS_PROVIDER : null;
        if (provider == null) return null;
        CountDownLatch done = new CountDownLatch(1);
        AtomicReference<Location> found = new AtomicReference<>();
        CancellationSignal cancel = new CancellationSignal();
        ExecutorService executor = Executors.newSingleThreadExecutor();
        try {
            manager.getCurrentLocation(provider, cancel, executor, location -> {
                found.set(location);
                done.countDown();
            });
            if (!done.await(remainingMs, TimeUnit.MILLISECONDS)) cancel.cancel();
        } catch (InterruptedException interrupted) {
            cancel.cancel();
            Thread.currentThread().interrupt();
        } finally {
            executor.shutdown();
        }
        return found.get();
    }
}
