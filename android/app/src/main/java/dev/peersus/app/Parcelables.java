package dev.peersus.app;

import android.os.Build;
import android.os.Bundle;
import android.os.Parcelable;

/** Typed IPC values, including the Android 12 versions supported by this application. */
final class Parcelables {
    private Parcelables() {}

    static <T extends Parcelable> T get(Bundle values, String key, Class<T> type) {
        if (Build.VERSION.SDK_INT >= 33) return values.getParcelable(key, type);
        return legacy(values, key, type);
    }

    // Android 12/12L have no typed overload. Keep the required compatibility call here,
    // not at every consumer; only this legacy branch needs the deprecation exemption.
    @SuppressWarnings("deprecation")
    private static <T extends Parcelable> T legacy(Bundle values, String key, Class<T> type) {
        Parcelable value = values.getParcelable(key);
        return type.isInstance(value) ? type.cast(value) : null;
    }
}
