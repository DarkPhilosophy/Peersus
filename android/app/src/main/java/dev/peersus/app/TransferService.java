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
    private native void asleepChanged(boolean asleep);

    /**
     * The phone sleeps while its screen is off. Doze is not read on purpose: it comes only after the screen has been off for a while,
     * and by then the system has already cut the network, so nothing could be said. The word leaves when the screen goes dark, while
     * the connection still works, and the other devices keep it for as long as the connection stays open.
     */
    private final android.content.BroadcastReceiver sleepWatch = new android.content.BroadcastReceiver() {
        @Override public void onReceive(Context context, Intent intent) { reportSleep(); }
    };

    private void reportSleep() {
        android.os.PowerManager power = getSystemService(android.os.PowerManager.class);
        if (power == null) return;
        asleepChanged(!power.isInteractive());
    }

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
        // Registered here and not in the manifest: the system delivers the screen broadcasts only to a receiver made at run time.
        android.content.IntentFilter filter = new android.content.IntentFilter(Intent.ACTION_SCREEN_OFF);
        filter.addAction(Intent.ACTION_SCREEN_ON);
        registerReceiver(sleepWatch, filter);
        // The state at start counts as well: a service that starts with the screen already off is asleep from the first moment.
        reportSleep();
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

    /** The notifications of the owner's list that reach outside the window, as JSON cards. Rust's. */
    public void setPostedNotifications(String cards) {
        PostedNotifications.update(this, cards);
    }

    /** The colours of the interface, given whenever they change and kept for the notification. Rust's. */
    public void setOfferAppearance(int[] colours) {
        OfferAppearance.keep(this, colours);
    }

    /** The folder chosen for the files that come in on their own, or "": asked with the window closed, when only the service is alive. Rust's. */
    public String savedFolder() {
        return SavedFolder.kept(this);
    }

    /** The same remote input as the activity's, for when only the service is alive. See {@link RemoteInput}. */
    public String injectRemoteInput(String batch) { return RemoteInput.inject(this, batch); }
    public void releaseRemoteInput(boolean keep) { RemoteInput.release(keep); }
    public String screenSize() { return RemoteInput.placeable(this); }

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

    /** Where the panel of an offer is drawn (0 the top, anything else the bottom), from the person's setting, given by Rust whenever it changes. */
    public void setNoticePlace(int place) {
        TransferNotifications.noticePlace(this, place);
    }

    /** The same Bluetooth as the activity's, for when only the service is alive. See {@link NearbyRadio}. */
    public int announceNearby(byte[] tag) { return NearbyRadio.announce(this, tag); }
    public int offerNearby(byte[] offer) { return NearbyRadio.offer(this, offer); }
    public int listenNearby(boolean on) { return NearbyRadio.listen(this, on); }

    /**
     * Puts a file that arrived whole into the folder the person chose (see {@link FolderDelivery}): with the window closed this is what ends an
     * Accept served without one, so that the file is in the folder and not left in the staging. Rust's, on any thread.
     */
    public void deliver(final String transfer, final String path, final String name, final String tree, final String[] folders) {
        FolderDelivery.deliver(this, transfer, path, name, tree, folders);
    }

    public void setLanguage(String code) { AppLanguage.update(this, code); }
    public String deviceLanguage() { return AppLanguage.device(); }
    /** Where this phone is now, for an extension allowed to know it; see {@link DeviceLocation#find}. Called from Rust, off the main thread. */
    public String location() { return DeviceLocation.find(this); }

    public void setBackgroundState(boolean keepAvailable, boolean transferring) {
        update(this, keepAvailable, transferring);
    }
    public void setRemoteInputWanted(boolean wanted) { InputRecoveryProvider.wanted(this, wanted); }

    /** Keeps store downloads available without an Activity. */
    public boolean downloadStore(String name, long limit, String output) {
        return StoreFetchBridge.download(this, name, limit, output);
    }
    /** The owner's verified update, with or without a window. See the distribution's {@link AppUpdate}. */
    public int installUpdate(String path, boolean reopen) { return AppUpdate.install(this, path, reopen); }
    public boolean updateManaged() { return AppUpdate.managed(); }
    /** The processors' ticks through the distribution's shell provider (see {@link ReadingsProvider}); null without one. Never waits. */
    public String processorTime() { return ReadingsProvider.processorTime(this); }

    public byte[] loadPrivateStore() throws java.io.IOException { return PrivateStoreConfig.load(this); }
    public boolean savePrivateStore(byte[] source) { return PrivateStoreConfig.save(this, source); }


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
        try { unregisterReceiver(sleepWatch); } catch (IllegalArgumentException notRegistered) { }
        synchronized (TransferService.class) {
            requested = false;
            promoted = false;
            reconcile(getApplicationContext());
        }
        super.onDestroy();
    }

    @Override public IBinder onBind(Intent intent) { return null; }
}
