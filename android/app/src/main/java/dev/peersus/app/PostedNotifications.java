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

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.util.HashSet;
import java.util.Set;

/**
 * The notifications of the owner's list that reach outside the window, shown by the system while the window is not in front. What they say,
 * who they are from and which of them are on show is Rust's (the same cards the window draws); this class only shows them and hands back a
 * press. Each is the system notification tagged with the owner's number, so one is never mistaken for another. A button answers with its id;
 * swiping it away closes it, as the cross of its card does. The window in front takes them all away: it shows the same cards itself.
 */
final class PostedNotifications {
    private static final String CHANNEL = "posted-notifications";
    private static final String TAG = "posted";
    // The cards as Rust last gave them: [{"id":n,"from":"…"|null,"text":"…","buttons":[{"id":"…","label":"…"}]}].
    private static JSONArray cards = new JSONArray();
    // The numbers the system shows now, so that one Rust took away is withdrawn.
    private static final Set<Long> shown = new HashSet<>();
    private static boolean visible;

    static synchronized void update(Context context, String json) {
        try {
            cards = new JSONArray(json == null ? "[]" : json);
        } catch (JSONException error) {
            android.util.Log.w("Peersus", "posted notifications: unreadable list");
            cards = new JSONArray();
        }
        show(context);
    }

    static synchronized void visible(Context context, boolean value) {
        visible = value;
        show(context);
    }

    static synchronized void languageChanged(Context context) {
        channel(context);
    }

    /** A button was pressed or the notification swiped away: it goes now, so that a second press cannot answer it again. */
    static void answered(Context context, long id) {
        context.getSystemService(NotificationManager.class).cancel(TAG + id, 0);
        synchronized (PostedNotifications.class) {
            shown.remove(id);
        }
    }

    private static void show(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        Set<Long> wanted = new HashSet<>();
        if (!visible && allowed(context)) {
            channel(context);
            for (int index = 0; index < cards.length(); index++) {
                JSONObject card = cards.optJSONObject(index);
                if (card == null || !card.has("id")) continue;
                long id = card.optLong("id");
                wanted.add(id);
                // Posted again only when new: the system would sound it again otherwise, and the cards do not change once posted.
                if (!shown.contains(id)) manager.notify(TAG + id, 0, build(context, id, card));
            }
        }
        for (Long id : shown) {
            if (!wanted.contains(id)) manager.cancel(TAG + id, 0);
        }
        shown.clear();
        shown.addAll(wanted);
    }

    private static Notification build(Context context, long id, JSONObject card) {
        String from = card.isNull("from") ? "Peersus" : card.optString("from");
        String text = card.optString("text");
        int code = Long.hashCode(id);
        int flags = PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT;
        PendingIntent open = PendingIntent.getActivity(context, code * 8,
                new Intent(context, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), flags);
        Notification.Builder notification = new Notification.Builder(context, CHANNEL)
                .setSmallIcon(R.drawable.ic_transfer_notification)
                .setContentTitle(from).setContentText(text)
                .setStyle(new Notification.BigTextStyle().bigText(text))
                .setContentIntent(open)
                .setDeleteIntent(PendingIntent.getBroadcast(context, code * 8 + 1, answer(context, id, null), flags))
                .setVisibility(Notification.VISIBILITY_PRIVATE)
                .setOnlyAlertOnce(true).setAutoCancel(false);
        JSONArray buttons = card.optJSONArray("buttons");
        // At most three, as the contract allows and the system draws.
        for (int index = 0; buttons != null && index < Math.min(3, buttons.length()); index++) {
            JSONObject button = buttons.optJSONObject(index);
            if (button == null) continue;
            PendingIntent does = PendingIntent.getBroadcast(context, code * 8 + 2 + index,
                    answer(context, id, button.optString("id")), flags);
            notification.addAction(new Notification.Action.Builder(null, button.optString("label"), does).build());
        }
        return notification.build();
    }

    private static Intent answer(Context context, long id, String button) {
        Intent intent = new Intent(context, NotificationActions.class).setAction(NotificationActions.ANSWER)
                .putExtra(NotificationActions.NOTIFICATION, id);
        if (button != null) intent.putExtra(NotificationActions.BUTTON, button);
        return intent;
    }

    private static void channel(Context context) {
        context.getSystemService(NotificationManager.class).createNotificationChannel(new NotificationChannel(CHANNEL,
                AppLanguage.context(context).getString(R.string.notices_channel), NotificationManager.IMPORTANCE_DEFAULT));
    }

    private static boolean allowed(Context context) {
        return Build.VERSION.SDK_INT < 33 || context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                == PackageManager.PERMISSION_GRANTED;
    }
}
