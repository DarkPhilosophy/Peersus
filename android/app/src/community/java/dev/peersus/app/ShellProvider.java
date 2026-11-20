package dev.peersus.app;

import android.os.RemoteException;
import moe.shizuku.server.IShizukuService;
import rikka.shizuku.Shizuku;

/** Check the provider itself: the SDK's cached permission cannot establish live authorization. */
final class ShellProvider {
    static boolean granted() {
        try {
            return Shizuku.pingBinder() && !Shizuku.isPreV11() && Shizuku.getUid() == 2000
                && IShizukuService.Stub.asInterface(Shizuku.getBinder()).checkSelfPermission();
        } catch (RemoteException | RuntimeException unavailable) { return false; }
    }
    private ShellProvider() { }
}
