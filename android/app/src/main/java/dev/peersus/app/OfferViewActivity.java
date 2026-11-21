package dev.peersus.app;

import android.app.Activity;
import android.os.Bundle;

/**
 * What a touch of the offer notification starts when the panel can be drawn. It shows nothing and ends at once: it exists
 * because the system closes the notification shade, and takes the notification that popped up off the screen, only when an
 * activity is started from it. A button that sent a broadcast left both where they were, drawn over the panel it had just opened.
 *
 * It is a task of its own that is kept in no list, so the window of the application is not brought forward by it.
 */
public final class OfferViewActivity extends Activity {
    @Override protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        TransferNotifications.viewFiles(getApplicationContext());
        // Before it is ever resumed, which is what a theme with no display requires.
        finish();
    }
}
