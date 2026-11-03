package dev.peersus.app;

import android.Manifest;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.view.View;
import android.widget.RemoteViews;

/**
 * The notification of an offer, drawn with the typeface and the colours of the application (the theme the person chose), with three buttons:
 * Cancel, View files and Accept. It answers the newest waiting offer. The lock screen shows the count only: the names of the files and the
 * identity of the sender are in the expanded notification, which the system does not draw on a locked screen.
 */
final class TransferNotifications {
    private static final String REQUESTS = "incoming-transfer-requests";
    private static final String RECEIVED = "received-files";
    static final int REQUEST_ID = 7102;
    private static final int RECEIVED_ID = 7103;
    private static boolean visible;
    static synchronized boolean hasPending() { return pending > 0; }
    private static int pending;
    // The offer the buttons answer. A notification posted for one offer must never answer another, so a new one re-issues it.
    private static String newest = "";
    // The files of every waiting offer, one line each (name and size), which View files shows. Kept so the notification can be made again.
    private static String files = "";
    private static int lastReceived;

    static synchronized void languageChanged(Context context) {
        channels(context);
        if (!visible && pending > 0) requests(context, pending, newest);
        if (!visible && lastReceived > 0 && allowed(context)) received(context, lastReceived);
    }

    /**
     * View files was pressed: the panel opens with every file of the offer and a box for each. The panel is drawn over other applications,
     * which the system lets only an application that was allowed to by hand, so when that is missing the person is taken to where they allow it.
     */
    static synchronized void viewFiles(Context context) {
        if (pending == 0) return;
        if (!android.provider.Settings.canDrawOverlays(context)) {
            context.startActivity(new Intent(android.provider.Settings.ACTION_MANAGE_OVERLAY_PERMISSION,
                    android.net.Uri.parse("package:" + context.getPackageName())).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            return;
        }
        OfferPanel.show(context, newest, files,
                AppLanguage.context(context).getResources().getQuantityString(R.plurals.requests_title, pending));
    }

    static synchronized void visible(Context context, boolean value) {
        visible = value;
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (value) {
            lastReceived = 0;
            manager.cancel(REQUEST_ID);
            manager.cancel(RECEIVED_ID);
        } else if (pending > 0) {
            requests(context, pending, newest);
        }
    }

    // Whether an Accept can be served by the owner with no window: saving on their own is on and a folder is chosen. Given by Rust whenever it
    // changes. It decides what the Accept button is made of, since that has to be settled when the notification is made: the system lets a
    // notification start the window only through the button's own intent (a press of the person), and blocks a window started later from the
    // background by a broadcast and a service.
    private static boolean acceptWithoutWindow;

    /** Kept for the next time the notification is made, and the notification that is shown is made again with it when it changed. */
    static synchronized void acceptWithoutWindow(Context context, boolean value) {
        if (acceptWithoutWindow == value) return;
        acceptWithoutWindow = value;
        if (pending > 0 && !visible) requests(context, pending, newest);
    }

    static synchronized void update(Context context, int waiting, int completed, String offer, String lines) {
        int previous = pending;
        String previousOffer = newest;
        pending = Math.max(0, waiting);
        newest = offer == null ? "" : offer;
        files = lines == null ? "" : lines;
        channels(context);
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        // Counts and flags only, so that a notification that did not come can be told from a call that never arrived.
        android.util.Log.i("Peersus", "notifications: waiting=" + waiting + " completed=" + completed + " previous=" + previous
                + " visible=" + visible + " allowed=" + allowed(context));
        // The notification is the announcement of an offer: made while one waits and the window is not in front, taken away when none waits.
        // The panel with the files to choose from opens from its View files button, and goes when the offer does or the window comes forward.
        if (pending == 0 || visible) {
            manager.cancel(REQUEST_ID);
            OfferPanel.hide(context);
        } else if (pending != previous || !newest.equals(previousOffer)) {
            requests(context, pending, newest);
            OfferPanel.refresh(context, newest, files, pending);
        }
        if (completed > 0 && !visible && allowed(context)) {
            lastReceived = completed;
            received(context, completed);
        }
    }

    private static void received(Context context, int count) {
        Context localized = AppLanguage.context(context);
        context.getSystemService(NotificationManager.class).notify(RECEIVED_ID, builder(context, RECEIVED)
                .setContentTitle(localized.getResources().getQuantityString(R.plurals.received_title, count))
                .setContentText(localized.getResources().getQuantityString(R.plurals.received_body, count, count))
                .setCategory(Notification.CATEGORY_STATUS).setOnlyAlertOnce(true).setAutoCancel(true).build());
    }

    private static void channels(Context context) {
        Context localized = AppLanguage.context(context);
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        manager.createNotificationChannel(new NotificationChannel(REQUESTS,
                localized.getString(R.string.requests_channel), NotificationManager.IMPORTANCE_HIGH));
        manager.createNotificationChannel(new NotificationChannel(RECEIVED,
                localized.getString(R.string.received_channel), NotificationManager.IMPORTANCE_DEFAULT));
    }

    private static void requests(Context context, int count, String offer) {
        channels(context);
        if (!allowed(context)) return;
        Context localized = AppLanguage.context(context);
        String title = localized.getResources().getQuantityString(R.plurals.requests_title, count);
        String body = localized.getResources().getQuantityString(R.plurals.requests_body, count, count);
        // The notification is made of the layout of the application, coloured with the theme in use. The system draws only the frame around
        // it, and keeps the plain look when the interface never gave its colours.
        RemoteViews layout = new RemoteViews(context.getPackageName(), R.layout.offer_notification);
        layout.setTextViewText(R.id.offer_title, title);
        layout.setTextViewText(R.id.offer_body, body);
        layout.setTextViewText(R.id.offer_cancel, localized.getString(R.string.offer_cancel));
        layout.setTextViewText(R.id.offer_view, localized.getString(R.string.offer_view));
        layout.setTextViewText(R.id.offer_accept, localized.getString(R.string.offer_accept));
        // The compact one, for when the notification is not expanded: a title and a line, since the full one is taller than the system leaves.
        RemoteViews compact = new RemoteViews(context.getPackageName(), R.layout.offer_notification_compact);
        compact.setTextViewText(R.id.offer_compact_title, title);
        compact.setTextViewText(R.id.offer_compact_body, body);
        int[] colours = OfferAppearance.kept(context);
        if (colours != null) {
            OfferAppearance.apply(layout, colours);
            OfferAppearance.applyCompact(compact, colours);
        }
        Notification.Builder notification = builder(context, REQUESTS)
                .setContentTitle(title).setContentText(body)
                .setStyle(new Notification.DecoratedCustomViewStyle())
                .setCustomContentView(compact).setCustomBigContentView(layout)
                .setCategory(Notification.CATEGORY_MESSAGE)
                .setNumber(count).setOnlyAlertOnce(true).setAutoCancel(false);
        if (!offer.isEmpty()) {
            // The request code is made from the offer and the button, so that the buttons of one offer are never the buttons of another.
            int code = offer.hashCode();
            int flags = PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT;
            Intent cancel = new Intent(context, NotificationActions.class).setAction(NotificationActions.CANCEL)
                    .putExtra(NotificationActions.TRANSFER, offer);
            // View files shows every file of the offer with a box for each. With the permission to draw over other applications that is the
            // panel, opened without the application; without it the system does not let a window of this application open from the
            // background (it blocked the screen of the permission when that was tried), so the button opens the application on the list.
            Intent view = android.provider.Settings.canDrawOverlays(context)
                    ? new Intent(context, NotificationActions.class).setAction(NotificationActions.VIEW).putExtra(NotificationActions.TRANSFER, offer)
                    : new Intent(context, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP)
                            .setAction(MainActivity.VIEW_OFFER).putExtra(NotificationActions.TRANSFER, offer);
            // Accept is answered by the owner with no window when it can be (saving on their own is on and a folder is chosen): a broadcast. When
            // the file has nowhere to go the window has to be started for the system's picker of the folder, and the system lets that happen
            // only from the intent of the button itself, so the button starts the window on that offer.
            Intent accept = acceptWithoutWindow
                    ? new Intent(context, NotificationActions.class).setAction(NotificationActions.ACCEPT).putExtra(NotificationActions.TRANSFER, offer)
                    : new Intent(context, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP)
                            .setAction(MainActivity.ACCEPT_OFFER).putExtra(NotificationActions.TRANSFER, offer);
            layout.setOnClickPendingIntent(R.id.offer_cancel, PendingIntent.getBroadcast(context, code * 31 + 1, cancel, flags));
            layout.setOnClickPendingIntent(R.id.offer_view, android.provider.Settings.canDrawOverlays(context)
                    ? PendingIntent.getBroadcast(context, code * 31 + 2, view, flags)
                    : PendingIntent.getActivity(context, code * 31 + 2, view, flags));
            layout.setOnClickPendingIntent(R.id.offer_accept, acceptWithoutWindow
                    ? PendingIntent.getBroadcast(context, code * 31 + 3, accept, flags)
                    : PendingIntent.getActivity(context, code * 31 + 3, accept, flags));
        }
        context.getSystemService(NotificationManager.class).notify(REQUEST_ID, notification.build());
    }

    private static boolean allowed(Context context) {
        return Build.VERSION.SDK_INT < 33 || context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                == PackageManager.PERMISSION_GRANTED;
    }

    private static Notification.Builder builder(Context context, String channel) {
        PendingIntent open = PendingIntent.getActivity(context, 0,
                new Intent(context, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Builder(context, channel)
                .setSmallIcon(R.drawable.ic_transfer_notification)
                .setContentIntent(open)
                .setVisibility(Notification.VISIBILITY_PRIVATE);
    }
}
