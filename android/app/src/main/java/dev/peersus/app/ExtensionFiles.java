package dev.peersus.app;

import android.app.Activity;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;

/** One explicit selection, one opened descriptor; never a reusable URI grant. */
final class ExtensionFiles {
    static final int REQUEST = 7006;
    private static final ThreadPoolExecutor WORK = new ThreadPoolExecutor(
            0, 1, 30, TimeUnit.SECONDS, new ArrayBlockingQueue<>(1),
            task -> new Thread(task, "peersus-extension-selection"));
    private final Activity activity;
    // Accessed only on the activity thread. Keep the picker occupied after cancellation
    // until its result arrives, so a stale result can never satisfy a newer request.
    private long pending;
    private boolean open;
    private boolean destroyed;
    private boolean saving;

    ExtensionFiles(Activity activity) { this.activity = activity; }

    /** Open the system picker for one file; {@code mime} narrows it to a kind ("image/*" opens the photo picker). */
    void choose(long request, String mime) {
        open(request, null, mime == null || mime.isEmpty() ? "*/*" : mime);
    }

    void save(long request, String name) {
        open(request, name, "application/octet-stream");
    }

    private void open(long request, String name, String mime) {
        activity.runOnUiThread(() -> {
            if (request <= 0 || destroyed || open) {
                selected(request, -1, null);
                return;
            }
            pending = request;
            open = true;
            saving = name != null;
            Intent intent;
            if (!saving && mime.equals("image/*") && android.os.Build.VERSION.SDK_INT >= 33) {
                // The system photo picker: recent photos and albums, with thumbnails.
                intent = new Intent(android.provider.MediaStore.ACTION_PICK_IMAGES);
            } else {
                intent = new Intent(saving ? Intent.ACTION_CREATE_DOCUMENT : Intent.ACTION_OPEN_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType(mime);
            }
            if (saving) {
                intent.putExtra(Intent.EXTRA_TITLE, name);
                intent.addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            } else {
                intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, false);
                intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            }
            try {
                activity.startActivityForResult(intent, REQUEST);
            } catch (RuntimeException unavailable) {
                pending = 0;
                open = false;
                selected(request, -1, null);
            }
        });
    }

    void cancel(long request) {
        activity.runOnUiThread(() -> {
            if (pending == request) pending = 0;
        });
    }

    void destroy() {
        destroyed = true;
        long request = pending;
        pending = 0;
        if (request != 0) selected(request, -1, null);
    }

    void result(int status, Intent data) {
        long request = pending;
        pending = 0;
        open = false;
        if (request == 0) return;
        Uri uri = status == Activity.RESULT_OK && data != null ? data.getData() : null;
        if (uri == null || destroyed) {
            selected(request, -1, null);
            return;
        }
        final boolean save = saving;
        try {
            WORK.execute(() -> {
                int detached = -1;
                String name = null;
                try {
                    if (save) {
                        // The system picker granted this specific new document, not its parent.
                        name = "";
                    } else {
                        try (Cursor cursor = activity.getContentResolver().query(uri,
                                new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
                            if (cursor != null && cursor.moveToFirst() && !cursor.isNull(0)) {
                                name = cursor.getString(0);
                            }
                        }
                    }
                    if (name != null) {
                        try (ParcelFileDescriptor descriptor = activity.getContentResolver()
                                .openFileDescriptor(uri, save ? "w" : "r")) {
                            if (descriptor != null) detached = descriptor.detachFd();
                        }
                    }
                } catch (Exception unreadable) {
                    // No URI, filename or exception is written to logs.
                }
                // Native code always takes ownership, including cancelled/unknown requests.
                selected(request, detached, name);
            });
        } catch (java.util.concurrent.RejectedExecutionException busy) {
            selected(request, -1, null);
        }
    }

    private static native void selected(long request, int descriptor, String name);
}
