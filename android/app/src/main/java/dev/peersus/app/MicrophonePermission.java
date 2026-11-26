package dev.peersus.app;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * Whether the system lets the application record from the microphone, for an extension the person allowed to record
 * ("audio.record"), and only when it starts a recording: the system's question is shown then, never at start. Called from Rust,
 * never on the main thread: it may wait for the person's answer. The recording itself is made in Rust.
 */
final class MicrophonePermission {
    /** Runtime permission result routed back by the activity to the waiting request. */
    static final int PERMISSION = 7006;
    /** The answers {@link #allow} gives. */
    static final String ALLOWED = "allowed";
    static final String REFUSED = "refused";
    static final String PENDING = "permission-pending";
    /** How long the person's answer is waited for: under the application's ceiling for the call. */
    private static final long WAIT_MS = 15 * 1000;
    private static final java.util.WeakHashMap<Activity, CountDownLatch> prompts = new java.util.WeakHashMap<>();

    private MicrophonePermission() {}

    static synchronized void permissionResult(Activity activity) {
        CountDownLatch pending = prompts.remove(activity);
        if (pending != null) pending.countDown();
    }

    private static boolean allowed(Context context) {
        return context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED;
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
                activity.requestPermissions(new String[] {Manifest.permission.RECORD_AUDIO}, PERMISSION);
            }
        });
        return pending;
    }

    /**
     * {@link #ALLOWED}; {@link #REFUSED} when the person's setting refuses (the system is asked only from the open window, where
     * the person can answer); {@link #PENDING} when the question is still open.
     */
    static String allow(Context context) {
        if (allowed(context)) return ALLOWED;
        if (!(context instanceof Activity)) return REFUSED;
        try {
            if (!prompt((Activity)context).await(WAIT_MS, TimeUnit.MILLISECONDS)) return PENDING;
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return PENDING;
        }
        return allowed(context) ? ALLOWED : REFUSED;
    }
}
