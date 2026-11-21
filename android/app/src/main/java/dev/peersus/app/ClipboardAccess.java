package dev.peersus.app;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipDescription;
import android.content.ClipboardManager;
import android.content.Context;
import android.os.PersistableBundle;
import java.lang.ref.WeakReference;
import java.nio.charset.StandardCharsets;

/** Main-thread-only access; elevated observation requires separate explicit provider consent. */
final class ClipboardAccess {
    // Binder parcels are deliberately much smaller than its shared transaction buffer.
    static final int MAX_BYTES = 64 * 1024;
    private static WeakReference<Activity> activity = new WeakReference<>(null);
    private static ClipboardManager manager;
    private static boolean resumed, focused, observing, listening, dirty = true;
    private static String cached;
    private static boolean reportedChange;
    private static final ClipboardConsent selection = new ClipboardConsent();
    private static long automaticSince = System.currentTimeMillis(), consumedTimestamp;
    private static long observationEpoch;
    private static ClipboardManager.OnPrimaryClipChangedListener changed;

    static void initialize(Context context) {
        if (manager == null) manager = (ClipboardManager) context.getApplicationContext().getSystemService(Context.CLIPBOARD_SERVICE);
        ClipboardProvider.initialize(context);
    }

    static void attach(Activity owner) {
        detach();
        activity = new WeakReference<>(owner);
        initialize(owner);
    }

    static void resume(Activity owner) {
        if (activity.get() != owner) attach(owner);
        resumed = true;
        focus(owner, owner.hasWindowFocus());
    }

    static void focus(Activity owner, boolean value) {
        if (activity.get() != owner) return;
        if (focused != value) resetEpoch();
        focused = value;
        updateListener();
        if (!value) clear();
    }

    static void pause(Activity owner) {
        if (activity.get() != owner) return;
        resumed = false;
        focused = false;
        resetEpoch();
        updateListener();
        cancelShared();
    }

    static void providerChanged(boolean fence) {
        if (fence) {
            automaticSince = System.currentTimeMillis();
            resetEpoch();
            updateListener();
        } else {
            clear();
            // A real change reported by the elevated source, which also works while this app is in the background.
            ExtensionClipboard.changed();
        }
    }

    static void detach(Activity owner) {
        if (activity.get() == owner) detach();
    }

    private static void detach() {
        if (listening && manager != null) manager.removePrimaryClipChangedListener(changed);
        listening = false;
        ++observationEpoch;
        resumed = focused = false;
        clear();
        cancelShared();
        activity.clear();
    }

    static void stopAutomatic() {
        observing = false;
        automaticSince = System.currentTimeMillis();
        resetEpoch();
        ClipboardProvider.observe(false);
        updateListener();
    }

    private static void resetEpoch() {
        if (listening && manager != null) manager.removePrimaryClipChangedListener(changed);
        listening = false;
        ++observationEpoch;
        clear();
    }

    private static void clear() {
        cached = null;
        dirty = true;
        reportedChange = false;
    }

    static boolean takeChanged() {
        boolean changed = reportedChange;
        reportedChange = false;
        return changed;
    }

    static void selectShared(long token, String text, long until) {
        selection.select(token, text, until);
        long remaining = Math.max(0, until - android.os.SystemClock.elapsedRealtime());
        new android.os.Handler(android.os.Looper.getMainLooper()).postDelayed(
            () -> selection.valid(token, android.os.SystemClock.elapsedRealtime()), remaining);
    }

    static String readSelected(long token) {
        if (!eligible()) { selection.clear(); return null; }
        return selection.take(token, android.os.SystemClock.elapsedRealtime());
    }

    static boolean selectedValid(long token) {
        return eligible() && selection.valid(token, android.os.SystemClock.elapsedRealtime());
    }

    static void cancelShared() { selection.clear(); }

    private static boolean eligible() {
        Activity owner = activity.get();
        return resumed && focused && owner != null && owner.hasWindowFocus() && manager != null;
    }

    private static void updateListener() {
        boolean wanted = observing && eligible();
        if (wanted == listening || manager == null) return;
        if (wanted) {
            long epoch = observationEpoch;
            changed = () -> {
                if (epoch != observationEpoch || !observing || !eligible()) return;
                dirty = true;
                cached = null;
                ExtensionClipboard.changed();
            };
            manager.addPrimaryClipChangedListener(changed);
        } else manager.removePrimaryClipChangedListener(changed);
        listening = wanted;
    }

    static boolean bounded(CharSequence text) {
        return text != null && text.length() <= MAX_BYTES
            && text.toString().getBytes(StandardCharsets.UTF_8).length <= MAX_BYTES;
    }

    static boolean freshCopy(long timestamp, long now, long consentSince, long consumed) {
        return timestamp > consentSince && timestamp > consumed && timestamp <= now;
    }

    static String read(boolean explicit) {
        reportedChange = false;
        if (explicit && !eligible()) { clear(); return null; }
        if (!explicit) {
            observing = true;
            ClipboardProvider.observe(true);
            updateListener();
            // Focus loss does not revoke opt-in. Read only copies newer than its fence,
            // including a copy made in another app while this Activity was away.
        }
        boolean elevated = !explicit && ClipboardProvider.active();
        if (!eligible() && !elevated) { clear(); return null; }
        if (!explicit && !dirty) return cached;
        try {
            if (!explicit && !elevated) {
                ClipDescription metadata = manager.getPrimaryClipDescription();
                if (metadata == null || !freshCopy(metadata.getTimestamp(), System.currentTimeMillis(), automaticSince, consumedTimestamp)) {
                    clear();
                    dirty = false;
                    return null;
                }
            }
            // The metadata check above is only an early rejection. Validate the actual clip again.
            long epoch = observationEpoch;
            ClipData clip = elevated ? ClipboardProvider.read() : manager.getPrimaryClip();
            if (epoch != observationEpoch || (elevated ? !ClipboardProvider.active() : !eligible())) return null;
            return acceptClip(clip, explicit);
        } catch (RuntimeException unavailable) {
            clear();
            return null;
        }
    }

    /** Same final validation after either OS source; called only after its authority/epoch checks. */
    static String acceptClip(ClipData clip, boolean explicit) {
        ClipDescription description = clip == null ? null : clip.getDescription();
        if (!explicit && (description == null || !freshCopy(description.getTimestamp(), System.currentTimeMillis(), automaticSince, consumedTimestamp))) {
            clear();
            dirty = false;
            return null;
        }
        String text = plainText(clip);
        if (!explicit) consumedTimestamp = description.getTimestamp();
        if (text == null) { clear(); dirty = false; return null; }
        if (!explicit) {
            cached = text;
            reportedChange = true;
            dirty = false;
        }
        return text;
    }

    static String plainText(ClipData clip) {
        if (clip == null || clip.getItemCount() != 1) return null;
        ClipDescription description = clip.getDescription();
        if (description == null || !description.hasMimeType("text/plain")) return null;
        PersistableBundle extras = description.getExtras();
        if (extras != null && extras.getBoolean("android.content.extra.IS_SENSITIVE", false)) return null;
        // Do not coerce a URI or Intent, even for explicit ordinary clipboard sends.
        if (clip.getItemAt(0).getUri() != null || clip.getItemAt(0).getIntent() != null) return null;
        CharSequence text = clip.getItemAt(0).getText();
        return bounded(text) ? text.toString() : null;
    }

    static boolean write(String text, java.util.function.BooleanSupplier authorized) {
        if (!bounded(text) || manager == null) return false;
        try {
            ClipData clip = ClipData.newPlainText("Peersus", text);
            PersistableBundle extras = new PersistableBundle();
            extras.putBoolean("android.content.extra.IS_SENSITIVE", true);
            clip.getDescription().setExtras(extras);
            // Check at the actual OS side-effect, after text validation/allocation.
            if (!authorized.getAsBoolean()) return false;
            cancelShared();
            manager.setPrimaryClip(clip);
            clear();
            return true;
        } catch (RuntimeException unavailable) { return false; }
    }
}
