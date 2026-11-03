package dev.peersus.app;

import android.app.Activity;
import android.content.ClipData;
import android.content.Context;

/** The Play distribution contains no elevated provider or hidden clipboard integration. */
final class ClipboardProvider {
    static void initialize(Context context) {}
    static boolean supported() { return false; }
    static boolean active() { return false; }
    static String status() { return "Unavailable in this distribution. Clipboard sending requires focus."; }
    static void open(Activity activity) {}
    static void observe(boolean wanted) {}
    static ClipData read() { return null; }
}
