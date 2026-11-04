package dev.peersus.app;

import android.content.Context;
import android.content.res.Configuration;
import java.util.Locale;

/** Rust settings are authoritative; this cache also localizes a service restarted without an Activity. */
final class AppLanguage {
    private static String code;

    static synchronized Context context(Context base) {
        if (code == null) code = base.getSharedPreferences("interface-language", Context.MODE_PRIVATE)
                .getString("code", "en");
        Configuration configuration = new Configuration(base.getResources().getConfiguration());
        configuration.setLocale(Locale.forLanguageTag(code));
        return base.createConfigurationContext(configuration);
    }
    static void update(Context context, String language) {
        synchronized (AppLanguage.class) {
            if (language.equals(code)) return;
            code = language;
            context.getSharedPreferences("interface-language", Context.MODE_PRIVATE).edit()
                    .putString("code", language).apply();
        }
        TransferNotifications.languageChanged(context);
        TransferService.notificationPermissionChanged(context);
    }
}
