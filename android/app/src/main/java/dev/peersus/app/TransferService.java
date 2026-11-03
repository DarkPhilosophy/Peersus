package dev.peersus.app;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.IBinder;

/** Keeps the process-owned network available for paired devices, independently of rendering. */
public final class TransferService extends Service {
    private static final String CHANNEL = "device-connectivity";
    private static final int NOTIFICATION = 7101;
    private static boolean visible;
    private static boolean background;
    private static boolean active;
    private static boolean requested;
    private static boolean promoted;
    private static boolean exhausted;

    static { System.loadLibrary("peersus_android"); }
    private native void startOwner(String directory);
    private native void releaseService();

    static synchronized void resumed(Context context) {
        visible = true;
        exhausted = false;
        reconcile(context.getApplicationContext());
        if (android.os.Build.VERSION.SDK_INT < 33
                || context.checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS)
                    == android.content.pm.PackageManager.PERMISSION_GRANTED) {
            notificationPermissionChanged(context);
        }
    }
    static synchronized boolean notificationsRelevant() { return background || active; }

    static synchronized void notificationPermissionChanged(Context context) {
        // A pending start will show the current notification when onCreate runs.
        if (requested && promoted && (background || active) && !exhausted) {
            try {
                context.startService(new Intent(context, TransferService.class));
            } catch (IllegalStateException | SecurityException refused) {
                android.util.Log.w("Peersus", "The system does not allow refreshing background connectivity");
            }
        }
    }

    static synchronized void paused() { visible = false; }

    static synchronized void update(Context context, boolean keepAvailable, boolean transferring) {
        background = keepAvailable;
        active = transferring;
        context.getSharedPreferences("background-service", MODE_PRIVATE).edit()
                .putBoolean("enabled", background).apply();
        reconcile(context.getApplicationContext());
    }

    private static void reconcile(Context context) {
        Intent intent = new Intent(context, TransferService.class);
        if (!background && !active) {
            // A queued foreground start must be promoted before it can be cancelled.
            // Keep requested until onDestroy so a new start cannot race the old teardown.
            if (requested && promoted) context.stopService(intent);
        } else if (visible && !requested && !exhausted) {
            // Android 12+ forbids an arbitrary background start. Arm while the user is here;
            // the existing service then continues when the activity pauses.
            try {
                requested = true;
                context.startForegroundService(intent);
            } catch (IllegalStateException | SecurityException refused) {
                requested = false;
                android.util.Log.w("Peersus", "Background device connectivity is unavailable");
            }
        }
    }

    @Override public void onCreate() {
        super.onCreate();
        synchronized (TransferService.class) {
            background = getSharedPreferences("background-service", MODE_PRIVATE).getBoolean("enabled", false);
            requested = true;
        }
        // Meet the foreground deadline BEFORE loading preferences or binding any network socket.
        if (!showForeground()) {
            // A sticky service the system brings back while the person is in another app may not become a foreground service: that is
            // refused, and an exception here ends the whole process. Stop instead, and let opening the app arm it again.
            synchronized (TransferService.class) { exhausted = true; }
            stopSelf();
            return;
        }
        // A sticky service can be recreated without an activity. Reuse or initialize the same Rust owner.
        PluginBridge.initialize(this);
        new Thread(() -> startOwner(getFilesDir().getAbsolutePath()), "peersus-owner-start").start();
    }

    /** Shows the notification that makes this a foreground service. False when the system does not allow it from where the app is now. */
    private boolean showForeground() {
        Context localized = AppLanguage.context(this);
        getSystemService(NotificationManager.class).createNotificationChannel(new NotificationChannel(CHANNEL,
                localized.getString(R.string.connectivity_channel), NotificationManager.IMPORTANCE_LOW));
        PendingIntent open = PendingIntent.getActivity(this, 0,
                new Intent(this, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification notification = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_transfer_notification)
                .setContentTitle(localized.getString(R.string.available_title))
                .setContentText(localized.getString(R.string.available_body))
                .setContentIntent(open)
                .setCategory(Notification.CATEGORY_SERVICE)
                .setVisibility(Notification.VISIBILITY_PRIVATE)
                .setOnlyAlertOnce(true)
                .setOngoing(true)
                .build();
        int type = android.os.Build.VERSION.SDK_INT >= 34
                ? ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE : 0;
        try {
            startForeground(NOTIFICATION, notification, type);
            synchronized (TransferService.class) { promoted = true; }
            return true;
        } catch (IllegalStateException | SecurityException refused) {
            android.util.Log.w("Peersus", "The system does not allow a foreground start from here");
            return false;
        }
    }
    public void setTransferNotifications(int waiting, int completed, String offer, String files) {
        TransferNotifications.update(this, waiting, completed, offer, files);
    }

    /** The colours of the interface, given whenever they change and kept for the notification. Rust's. */
    public void setOfferAppearance(int[] colours) {
        OfferAppearance.keep(this, colours);
    }

    /** The folder chosen for the files that come in on their own, or "": asked with the window closed, when only the service is alive. Rust's. */
    public String savedFolder() {
        return SavedFolder.kept(this);
    }

    /**
     * Brings the window of the program forward, for what only a window can do (the system's picker of the folder a file is saved in). Called
     * by Rust after the person pressed Accept, which is what lets a program start a window from the background.
     */
    public void bringWindowForward() {
        Intent open = new Intent(this, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        startActivity(open);
    }

    /** Whether an Accept is served by the owner with no window (saving on their own is on and a folder is chosen), given by Rust whenever it changes. */
    public void setAcceptWithoutWindow(boolean value) {
        TransferNotifications.acceptWithoutWindow(this, value);
    }

    /**
     * Puts a file that arrived whole into the folder the person chose (see {@link FolderDelivery}): with the window closed this is what ends an
     * Accept served without one, so that the file is in the folder and not left in the staging. Rust's, on any thread.
     */
    public void deliver(final String transfer, final String path, final String name, final String tree, final String[] folders) {
        FolderDelivery.deliver(this, transfer, path, name, tree, folders);
    }

    public void setLanguage(String code) { AppLanguage.update(this, code); }

    public void setBackgroundState(boolean keepAvailable, boolean transferring) {
        update(this, keepAvailable, transferring);
    }


    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        synchronized (TransferService.class) {
            // Even a cancelled start must first discharge Android's foreground deadline.
            if (!showForeground()) {
                exhausted = true;
                stopSelf(startId);
                return START_NOT_STICKY;
            }
            if ((!background && !active) || exhausted) {
                stopForeground(STOP_FOREGROUND_REMOVE);
                promoted = false;
                stopSelf(startId);
                return START_NOT_STICKY;
            }
            return background ? START_STICKY : START_NOT_STICKY;
        }
    }

    @Override public void onTimeout(int startId, int foregroundServiceType) {
        synchronized (TransferService.class) { exhausted = true; }
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
        MainActivity.backgroundTimedOut();
    }

    @Override public void onDestroy() {
        releaseService();
        synchronized (TransferService.class) {
            requested = false;
            promoted = false;
            reconcile(getApplicationContext());
        }
        super.onDestroy();
    }

    @Override public IBinder onBind(Intent intent) { return null; }
}
