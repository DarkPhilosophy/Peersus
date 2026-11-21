package dev.peersus.app;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * Brings the background service back after the phone starts again and after the application is updated, when the person asked for it to
 * run in the background. Both end the process and with it the service, and without this nothing starts it again until the window is opened.
 *
 * These two broadcasts are among the few moments the system lets an application start a foreground service while it is not on screen, and
 * the type this service is declared with (a connected device) is not one of those it refuses to start from a boot broadcast. Nothing is
 * started when the option is off: the choice is read from the same place the service writes it.
 */
public final class BootStart extends BroadcastReceiver {
    @Override public void onReceive(Context context, Intent intent) {
        if (intent == null) return;
        String action = intent.getAction();
        if (!Intent.ACTION_BOOT_COMPLETED.equals(action) && !Intent.ACTION_MY_PACKAGE_REPLACED.equals(action)) return;
        if (!context.getSharedPreferences("background-service", Context.MODE_PRIVATE).getBoolean("enabled", false)) return;
        try {
            context.startForegroundService(new Intent(context, TransferService.class));
        } catch (IllegalStateException | SecurityException refused) {
            // The system did not allow it from here: the window arms the service again the next time it is opened.
            android.util.Log.w("Peersus", "The background service could not be started after the restart");
        }
    }
}
