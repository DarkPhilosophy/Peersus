package dev.peersus.app;

import android.app.NotificationManager;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * The buttons of the offer notification. They work with the window closed: the process is started by the broadcast if it is not running, and
 * what was pressed is handed to Rust, which answers that one offer. Accept does not open the window by itself: with saving on their own on and a
 * folder chosen, Rust receives the file with no window at all, and only when it has nowhere to put it does Rust bring the window forward, for
 * the system's picker of the folder, which only a window can show.
 */
public final class NotificationActions extends BroadcastReceiver {
    static final String CANCEL = "dev.peersus.app.CANCEL_OFFER";
    static final String ACCEPT = "dev.peersus.app.ACCEPT_OFFER_NOW";
    static final String VIEW = "dev.peersus.app.VIEW_OFFER_FILES";
    static final String TRANSFER = "transfer";

    static { System.loadLibrary("peersus_android"); }

    /** What was pressed ("cancel" or "accept") and the offer it was made for. Rust's. */
    static native void offerAnswered(String kind, String transfer);

    @Override public void onReceive(Context context, Intent intent) {
        if (intent == null) return;
        if (VIEW.equals(intent.getAction())) {
            // The panel with the files to choose from opens over what the person is doing: the application does not open.
            TransferNotifications.viewFiles(context);
            return;
        }
        if (ACCEPT.equals(intent.getAction())) {
            String transfer = intent.getStringExtra(TRANSFER);
            if (transfer != null && !transfer.isEmpty()) offerAnswered("accept", transfer);
            return;
        }
        if (!CANCEL.equals(intent.getAction())) return;
        String transfer = intent.getStringExtra(TRANSFER);
        if (transfer == null || transfer.isEmpty()) return;
        offerAnswered("cancel", transfer);
        // The offer is answered: its notification goes, so that a second press cannot answer a thing that is already answered.
        context.getSystemService(NotificationManager.class).cancel(TransferNotifications.REQUEST_ID);
    }
}
