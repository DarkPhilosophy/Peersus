package dev.peersus.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.Intent;
import android.provider.Settings;

/** No elevated provider exists in this distribution. Android approval remains manual. */
final class InputRecoveryProvider {
    static void initialize(Context owner) { }
    private static volatile boolean wanted;
    private static boolean prompted;
    static void request() { }
    static void wanted(Context owner, boolean value) { wanted = value; prompted = false; }
    static void resumed(Activity owner) {
        if (!wanted || prompted || RemoteInput.connected()) return;
        prompted = true;
        open(owner);
    }
    static boolean on() { return false; }
    static void set(Activity owner, boolean value) { if (value) open(owner); }
    static String status() {
        return RemoteInput.connected() ? "Accessibility service connected." : "Accessibility service not connected. Enable it in Android settings.";
    }
    static void open(Activity owner) {
        new AlertDialog.Builder(owner).setTitle("Android accessibility settings")
            .setMessage("Automatic Stellar/Shizuku recovery is not supported in this distribution. Enable or reconnect the remote input service manually in Android settings.")
            .setPositiveButton("Android settings", (dialog, which) ->
                owner.startActivity(new Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)))
            .setNegativeButton("Cancel", null).show();
    }
    private InputRecoveryProvider() { }
}
