package dev.peersus.app;

import android.app.Activity;
import android.os.SystemClock;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Generic clipboard facility for sandboxed extensions, owned by the application process.
 * It names no extension: every decision about when text may be read stays in
 * {@link ClipboardAccess}. Each call runs on the activity thread and answers exactly once,
 * and a call whose caller was cancelled or whose deadline passed never touches the clipboard.
 */
final class ExtensionClipboard {
    static final int READ = 1, OBSERVE = 2, WRITE = 3, RESET = 4;
    private static final long MAXIMUM_WAIT = 2500;
    private static final int MAXIMUM_PENDING = 8;
    private final Activity activity;
    private final ConcurrentHashMap<Long, Request> pending = new ConcurrentHashMap<>();

    ExtensionClipboard(Activity activity) { this.activity = activity; }

    /** One admitted request: cancellation is visible to the activity thread before it acts. */
    private static final class Request {
        final long deadline;
        final AtomicBoolean live = new AtomicBoolean(true);
        Request(long deadline) { this.deadline = deadline; }
        boolean allowed() { return live.get() && SystemClock.elapsedRealtime() < deadline; }
    }

    /** Admits the request, then runs it on the activity thread; the answer returns through {@link #answered}. */
    void run(long id, long remaining, int operation) { run(id, remaining, operation, null); }

    void run(long id, long remaining, int operation, String text) {
        boolean admitted = id > 0 && remaining > 0 && remaining <= MAXIMUM_WAIT
            && pending.size() < MAXIMUM_PENDING
            && pending.putIfAbsent(id, new Request(SystemClock.elapsedRealtime() + remaining)) == null;
        if (!admitted) { answered(id, operation, false, null, false); return; }
        Request request = pending.get(id);
        activity.runOnUiThread(() -> {
            try {
                if (request == null || !request.allowed()) { answered(id, operation, false, null, false); return; }
                switch (operation) {
                    case READ: {
                        String value = ClipboardAccess.read(true);
                        answered(id, operation, value != null, value, false);
                        break;
                    }
                    case WRITE:
                        answered(id, operation, ClipboardAccess.write(text, request::allowed), null, false);
                        break;
                    case OBSERVE: {
                        String value = ClipboardAccess.read(false);
                        answered(id, operation, value != null, value, value != null && ClipboardAccess.takeChanged());
                        break;
                    }
                    case RESET:
                        ClipboardAccess.stopAutomatic();
                        answered(id, operation, true, null, false);
                        break;
                    default:
                        answered(id, operation, false, null, false);
                }
            } catch (RuntimeException unavailable) {
                answered(id, operation, false, null, false);
            } finally {
                pending.remove(id);
            }
        });
    }

    /** The activity is going away: answer every waiting request so none hangs until its deadline. */
    void destroy() {
        for (Long id : pending.keySet()) {
            Request request = pending.remove(id);
            if (request != null) {
                request.live.set(false);
                answered(id, 0, false, null, false);
            }
        }
    }

    void cancel(long id) {
        Request request = pending.remove(id);
        if (request != null) request.live.set(false);
    }

    private static native void answered(long id, int operation, boolean ok, String text, boolean changed);

    /** Called wherever the platform policy notices a change; only the fact that something changed leaves Java. */
    static void changed() { notifyChanged(); }
    private static native void notifyChanged();
}
