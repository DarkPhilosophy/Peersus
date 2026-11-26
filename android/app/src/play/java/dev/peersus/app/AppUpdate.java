package dev.peersus.app;

import android.app.Activity;
import android.content.Context;
import android.content.IntentSender;
import android.content.SharedPreferences;
import com.google.android.play.core.appupdate.AppUpdateManager;
import com.google.android.play.core.appupdate.AppUpdateManagerFactory;
import com.google.android.play.core.appupdate.AppUpdateOptions;
import com.google.android.play.core.install.model.AppUpdateType;
import com.google.android.play.core.install.model.InstallStatus;
import com.google.android.play.core.install.model.UpdateAvailability;

/**
 * This distribution is kept up to date by Google Play and never replaces itself. When Play has a newer version, Play's own question is
 * shown once for that version while the window is open; the person decides, Play downloads and verifies, and the update Play holds
 * ready is put in place the next time the window is opened.
 */
final class AppUpdate {
    private static final int REQUEST = 7243;

    /** Whether something else than the application keeps this installation up to date. */
    static boolean managed() { return true; }

    /** Nothing is installed from here in this distribution. */
    static int install(Context owner, String path, boolean reopen) { return 6; }

    static void resumed(Activity owner) {
        final AppUpdateManager manager;
        try { manager = AppUpdateManagerFactory.create(owner.getApplicationContext()); }
        catch (RuntimeException unavailable) { return; }
        manager.getAppUpdateInfo().addOnSuccessListener(info -> {
            if (owner.isFinishing() || owner.isDestroyed()) return;
            if (info.installStatus() == InstallStatus.DOWNLOADED) {
                manager.completeUpdate();
                return;
            }
            if (info.updateAvailability() != UpdateAvailability.UPDATE_AVAILABLE
                    || !info.isUpdateTypeAllowed(AppUpdateType.FLEXIBLE)) return;
            SharedPreferences kept = owner.getSharedPreferences("app-update", Context.MODE_PRIVATE);
            int offered = info.availableVersionCode();
            if (kept.getInt("asked", 0) >= offered) return;
            try {
                if (manager.startUpdateFlowForResult(info, owner,
                        AppUpdateOptions.newBuilder(AppUpdateType.FLEXIBLE).build(), REQUEST)) {
                    kept.edit().putInt("asked", offered).apply();
                }
            } catch (IntentSender.SendIntentException | RuntimeException refused) {
                // Play could not show its question now; it is asked again the next time the window opens.
            }
        });
    }
    private AppUpdate() { }
}
