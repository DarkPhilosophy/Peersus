package dev.peersus.app;

import android.content.Context;

/** Play's compile-time boundary: no loader, no native services, no external execution. */
final class NativeModules {
    static boolean launch(Context context, long generation, String module, String control, String data, String token) { return false; }
    static void stop(long generation) { }
}
