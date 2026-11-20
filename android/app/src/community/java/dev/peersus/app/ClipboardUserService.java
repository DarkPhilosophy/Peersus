package dev.peersus.app;

import android.content.ClipData;
import android.content.Context;
import android.os.Binder;
import android.os.IBinder;
import android.os.Parcel;
import android.os.RemoteException;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;

/** Runs only after the provider's interactive grant. Shell owns both subscription and reads. */
public final class ClipboardUserService extends IClipboardSource.Stub {
    private final int ownerUid;
    private Object clipboard, listener;
    private Class<?> listenerType;
    private IClipboardChange callback;
    private int user;
    private final IBinder.DeathRecipient ownerDied = this::close;

    public ClipboardUserService(Context context) {
        ownerUid = context.getApplicationInfo().uid;
    }

    private void enforceOwner() {
        if (Binder.getCallingUid() != ownerUid || android.os.Process.myUid() != 2000)
            throw new SecurityException("Clipboard source is limited to its owner and shell provider");
    }

    @Override public synchronized boolean start(int requestedUser, IClipboardChange target) {
        enforceOwner();
        close();
        if (target == null || requestedUser != ownerUid / 100000) return false;
        user = requestedUser;
        try {
            IBinder service = (IBinder) Class.forName("android.os.ServiceManager")
                .getMethod("getService", String.class).invoke(null, "clipboard");
            clipboard = Class.forName("android.content.IClipboard$Stub")
                .getMethod("asInterface", IBinder.class).invoke(null, service);
            listenerType = Class.forName("android.content.IOnPrimaryClipChangedListener");
            Binder notifications = new Binder() {
                @Override protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) {
                    if (code == INTERFACE_TRANSACTION) {
                        if (reply != null) reply.writeString("android.content.IOnPrimaryClipChangedListener");
                        return true;
                    }
                    if (code != FIRST_CALL_TRANSACTION) return false;
                    data.enforceInterface("android.content.IOnPrimaryClipChangedListener");
                    if (Binder.getCallingUid() != 1000) return false;
                    synchronized (ClipboardUserService.this) {
                        if (callback != null) try { callback.changed(); }
                        catch (RemoteException dead) { close(); }
                    }
                    return true;
                }
            };
            listener = Proxy.newProxyInstance(listenerType.getClassLoader(), new Class<?>[]{listenerType},
                (proxy, method, args) -> {
                    if (method.getName().equals("asBinder")) return notifications;
                    if (method.getName().equals("toString")) return "Clipboard listener";
                    if (method.getName().equals("hashCode")) return System.identityHashCode(proxy);
                    if (method.getName().equals("equals")) return proxy == args[0];
                    return null;
                });
            callback = target;
            callback.asBinder().linkToDeath(ownerDied, 0);
            call("addPrimaryClipChangedListener", true);
            return true;
        } catch (ReflectiveOperationException | RuntimeException | RemoteException unavailable) {
            close();
            return false;
        }
    }

    /** Resolve only the known 12–16 signatures; never guess transaction numbers. */
    private Object call(String name, boolean withListener) throws ReflectiveOperationException {
        for (Method method : Class.forName("android.content.IClipboard").getMethods()) {
            if (!method.getName().equals(name)) continue;
            Class<?>[] types = method.getParameterTypes();
            int offset = withListener ? 1 : 0;
            if (types.length < offset + 2 || types.length > offset + 4) continue;
            if (withListener && types[0] != listenerType) continue;
            if (types[offset] != String.class) continue;
            Object[] args = new Object[types.length];
            if (withListener) args[0] = listener;
            args[offset] = "com.android.shell";
            int next = offset + 1;
            if (types[next] == String.class) args[next++] = null; // Attribution tag, when present.
            if (next >= types.length || types[next] != int.class) continue;
            args[next++] = user;
            if (next < types.length && types[next] == int.class) args[next++] = 0; // Default physical device.
            if (next != types.length) continue;
            long identity = Binder.clearCallingIdentity();
            try { return method.invoke(clipboard, args); }
            finally { Binder.restoreCallingIdentity(identity); }
        }
        throw new NoSuchMethodException("Unsupported clipboard interface");
    }

    @Override public synchronized ClipData read() {
        enforceOwner();
        if (callback == null || !callback.asBinder().isBinderAlive()) {
            close();
            throw new IllegalStateException("Clipboard source stopped");
        }
        try {
            ClipData clip = (ClipData) call("getPrimaryClip", false);
            // The application applies this same policy and its consent fence again after IPC.
            if (ClipboardAccess.plainText(clip) == null) return null;
            Parcel size = Parcel.obtain();
            try {
                clip.writeToParcel(size, 0);
                return size.dataSize() <= 160 * 1024 ? clip : null;
            } finally { size.recycle(); }
        } catch (ReflectiveOperationException | RuntimeException unavailable) {
            close();
            throw new IllegalStateException("Clipboard source unavailable");
        }
    }

    @Override public synchronized void stop() { enforceOwner(); close(); }

    private synchronized void close() {
        if (clipboard != null && listener != null) try { call("removePrimaryClipChangedListener", true); }
        catch (ReflectiveOperationException | RuntimeException ignored) {}
        if (callback != null) try { callback.asBinder().unlinkToDeath(ownerDied, 0); }
        catch (java.util.NoSuchElementException ignored) {}
        callback = null;
        clipboard = listener = null;
    }

    @Override public void destroy() {
        // Reserved Shizuku lifecycle transaction is sent by its shell process.
        if (Binder.getCallingUid() != android.os.Process.myUid() && Binder.getCallingUid() != ownerUid) throw new SecurityException();
        close();
        System.exit(0);
    }
}
