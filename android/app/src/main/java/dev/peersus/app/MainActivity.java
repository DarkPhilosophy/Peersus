package dev.peersus.app;

import android.app.NativeActivity;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Insets;
import android.net.Uri;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import java.util.ArrayList;
import java.util.List;

// The interface is drawn by Rust onto a SurfaceView. This class only gives that surface keyboard focus, which
// a NativeActivity does not do on its own, so that the soft keyboard has somewhere to type. It is the same
// strategy as the upstream toolkit's own host. No application logic is written in Java.
public class MainActivity extends NativeActivity {
    static {
        // The base class loads this library for its own entry point, which does not bind the native methods of this
        // subclass: the runtime looks for them through the class that declares them. Loaded here too, it is the same
        // library, found already loaded, and its symbols are bound to this class before the first of them is called.
        System.loadLibrary("peersus_android");
    }

    private View findSurface(View view) {
        if (view instanceof SurfaceView) return view;
        if (view instanceof ViewGroup) {
            ViewGroup group = (ViewGroup) view;
            for (int i = 0; i < group.getChildCount(); i++) {
                View found = findSurface(group.getChildAt(i));
                if (found != null) return found;
            }
        }
        return null;
    }

    @Override
    public void onCreate(Bundle state) {
        PluginBridge.attach(this);
        super.onCreate(state);
        if (state != null) documentGeneration = state.getLong("pickerGeneration", 0);
        if (state != null) pluginPickerOpen = state.getBoolean("pluginPickerOpen", false);
        View decor = getWindow().getDecorView();
        // From targetSdk 35 the system draws the window edge to edge. This listener makes the system report the
        // usable area, without the status bar, the navigation bar and the keyboard, as the activity's content
        // rectangle; the interface (Rust) reads that rectangle and keeps its own content inside it, at the
        // window's own scale. Nothing about the bars is written down here or there: the system says.
        decor.setOnApplyWindowInsetsListener((view, insets) -> {
            Insets bars = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.ime()
                    | WindowInsets.Type.displayCutout());
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom);
            // The very same bars, kept for Rust to ask for (see `usableAreaMargins`): one source, written where it is applied.
            usableMargins = new int[] {bars.left, bars.top, bars.right, bars.bottom};
            return WindowInsets.CONSUMED;
        });
        decor.requestApplyInsets();
        decor.post(() -> {
            View surface = findSurface(decor);
            if (surface != null) {
                surface.setFocusable(true);
                surface.setFocusableInTouchMode(true);
                surface.requestFocus();
            }
        });
        handleOfferIntent(getIntent());
        PluginBridge.offerSharedText(this, getIntent());
        handleSharedFiles(getIntent());
    }

    @Override
    protected void onSaveInstanceState(Bundle state) {
        state.putLong("pickerGeneration", documentGeneration);
        state.putBoolean("pluginPickerOpen", pluginPickerOpen);
        super.onSaveInstanceState(state);
    }

    @Override protected void onResume() {
        super.onResume();
        ClipboardAccess.resume(this);
        TransferService.resumed(this);
        TransferNotifications.visible(this, true);
    }

    /** Called by the process-owned runtime, not the rendering/activity loop. */
    public void setBackgroundState(boolean background, boolean active) {
        TransferService.update(this, background, active);
        if (background || active) runOnUiThread(this::requestTransferNotifications);
    }

    /** Called only from the native owner's blocking pool, never from the UI thread. */
    public boolean downloadStore(String name, long limit, String output) {
        return StoreFetchBridge.download(this, name, limit, output);
    }

    public void setTransferNotifications(int waiting, int completed, String offer, String files) {
        TransferNotifications.update(this, waiting, completed, offer, files);
        if (waiting > 0) runOnUiThread(this::requestTransferNotifications);
    }

    /** The colours of the interface, given whenever they change and kept for the notification. Rust's. */
    public void setOfferAppearance(int[] colours) {
        OfferAppearance.keep(this, colours);
    }

    /** The action of the View files button of the offer notification, when the application is what shows the files. */
    static final String VIEW_OFFER = "dev.peersus.app.VIEW_OFFER";
    /**
     * The action of the Accept button of the offer notification, when the file has nowhere to go (no folder is chosen, or saving on their own is
     * off) and so the window has to ask for one: the system lets a notification start a window only through the intent of its own button.
     */
    static final String ACCEPT_OFFER = "dev.peersus.app.ACCEPT_OFFER";

    @Override protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleOfferIntent(intent);
        PluginBridge.offerSharedText(this, intent);
        handleSharedFiles(intent);
    }

    /** Explicit support-link activation only. No arbitrary intent schemes or caller-supplied authorities. */
    public boolean openExternalUrl(String value) {
        if (!"https://ko-fi.com/DarkPhilosophy".equals(value)) return false;
        android.os.Handler main = new android.os.Handler(android.os.Looper.getMainLooper());
        Runnable open = () -> {
            try {
                startActivity(new Intent(Intent.ACTION_VIEW, android.net.Uri.parse(value))
                        .addCategory(Intent.CATEGORY_BROWSABLE));
            } catch (android.content.ActivityNotFoundException | SecurityException unavailable) {
                android.widget.Toast.makeText(this, "No browser is available. Open the displayed address manually.", android.widget.Toast.LENGTH_LONG).show();
            }
        };
        if (android.os.Looper.myLooper() == android.os.Looper.getMainLooper()) { open.run(); return true; }
        return main.post(open);
    }

    /**
     * A button of the notification started the window: the offer it was made for is handed to Rust, whose window then shows the list of the files
     * (View files) or the folder picker (Accept), as if the person had pressed that button on the offer bar. The action is taken off the intent,
     * so that the window being made again (turned, or restored) does not do it a second time. An Accept that needs no window is not here: it is
     * answered by the owner (see {@link NotificationActions}).
     */
    private void handleOfferIntent(Intent intent) {
        if (intent == null) return;
        String action = intent.getAction();
        String kind = VIEW_OFFER.equals(action) ? "view" : ACCEPT_OFFER.equals(action) ? "accept" : null;
        if (kind == null) return;
        String transfer = intent.getStringExtra(NotificationActions.TRANSFER);
        intent.setAction(Intent.ACTION_MAIN);
        intent.removeExtra(NotificationActions.TRANSFER);
        if (transfer != null && !transfer.isEmpty()) NotificationActions.offerAnswered(kind, transfer);
    }

    /** Whether an Accept is served by the owner with no window (saving on their own is on and a folder is chosen), given by Rust whenever it changes. */
    public void setAcceptWithoutWindow(boolean value) {
        TransferNotifications.acceptWithoutWindow(this, value);
    }

    public void setLanguage(String code) { AppLanguage.update(this, code); }

    public boolean backgroundClipboardSupported() { return ClipboardProvider.supported(); }
    public String backgroundClipboardStatus() { return ClipboardProvider.status(); }
    public void openBackgroundClipboard() { runOnUiThread(() -> ClipboardProvider.open(this)); }


    /** Whether the person was already taken to the settings of the notifications during this run of the process. */
    private static boolean sentToSettings;

    private void requestTransferNotifications() {
        if (android.os.Build.VERSION.SDK_INT < 33 || !hasWindowFocus()
                || checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS)
                    == android.content.pm.PackageManager.PERMISSION_GRANTED) return;
        android.content.SharedPreferences preferences = getSharedPreferences("notification-consent", MODE_PRIVATE);
        if (preferences.getBoolean("asked", false)) {
            // Refused before: the system will not ask again, so an offer that waits for the person is never told to them, and nothing said why.
            // Take them to where it can be allowed, once for each run of the process, and only when an offer is actually waiting.
            if (TransferNotifications.hasPending() && !sentToSettings) {
                sentToSettings = true;
                try {
                    startActivity(new Intent(android.provider.Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                            .putExtra(android.provider.Settings.EXTRA_APP_PACKAGE, getPackageName()));
                } catch (RuntimeException unavailable) {
                    android.util.Log.w("Peersus", "The settings of the notifications could not be opened");
                }
            }
            return;
        }
        preferences.edit().putBoolean("asked", true).apply();
        requestPermissions(new String[] {android.Manifest.permission.POST_NOTIFICATIONS}, 7004);
    }

    /** Whether the panel of an offer can be drawn over other applications: the person allowed it by hand, in the system's settings. */
    public boolean overlayAllowed() {
        return android.provider.Settings.canDrawOverlays(this);
    }

    /** The margins the system's bars, the keyboard and the cutout take, as the window applied them last: left, top, right, bottom, in pixels. */
    private volatile int[] usableMargins = new int[] {0, 0, 0, 0};

    /**
     * The usable area as a rectangle (left, top, right, bottom, in pixels of the screen) for Rust to ask from a thread of its own, the same thing the
     * native application state reports as its content rectangle: the window minus the bars, the keyboard and the cutout. All zero until the window has
     * a size and the system has said what it takes ("not known yet"). Never read from the native application state: that is destroyed with the
     * activity, and a thread that reads it afterwards takes a lock that no longer exists and the process is aborted.
     */
    public int[] usableAreaRectangle() {
        View decor = getWindow().getDecorView();
        int[] margins = usableMargins;
        int width = decor.getWidth();
        int height = decor.getHeight();
        if (width <= 0 || height <= 0) return new int[] {0, 0, 0, 0};
        return new int[] {margins[0], margins[1], width - margins[2], height - margins[3]};
    }

    /**
     * Whether the system is in its dark mode now, from the configuration the resources of this activity have. Asked by Rust from a thread of its
     * own, which must never read it from the native application state: that state is torn down with the activity, and a thread that reads it
     * afterwards takes a lock that no longer exists and the process is aborted.
     */
    public boolean darkModeNow() {
        int night = getResources().getConfiguration().uiMode & android.content.res.Configuration.UI_MODE_NIGHT_MASK;
        return night == android.content.res.Configuration.UI_MODE_NIGHT_YES;
    }

    /**
     * Opens the screen of the system where drawing over other applications is allowed for this program. It is not a permission the program can
     * ask for with a dialog, only one the person gives on that screen. Called from Rust, on any thread.
     */
    public void openOverlayPermission() {
        runOnUiThread(() -> startActivity(new Intent(android.provider.Settings.ACTION_MANAGE_OVERLAY_PERMISSION,
                android.net.Uri.parse("package:" + getPackageName()))));
    }

    /**
     * Whether the system leaves this program out of its battery optimisation, which is what keeps the network of the background service
     * working while the phone is idle. The person decides it in the system's settings; the program does not ask for it directly.
     */
    public boolean batteryUnrestricted() {
        android.os.PowerManager power = getSystemService(android.os.PowerManager.class);
        return power != null && power.isIgnoringBatteryOptimizations(getPackageName());
    }

    /**
     * Opens the system's list of programs and their battery optimisation, where the person chooses. It is the list and not the direct
     * request, which the store of applications limits to a few kinds of program. Called from Rust, on any thread.
     */
    public void openBatterySettings() {
        runOnUiThread(() -> startActivity(new Intent(android.provider.Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)));
    }

    /** Whether notifications of this application can be shown now: the permission is given and the application is not blocked as a whole. */
    public boolean notificationsAllowed() {
        if (android.os.Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            return false;
        }
        return getSystemService(android.app.NotificationManager.class).areNotificationsEnabled();
    }

    /**
     * What the notifications row of Settings does when it is pressed. If the system will still ask (it was never asked, or asked only once), it asks;
     * if it will not ask again, or the permission is given but the application is blocked in the settings, it opens the notification settings of this
     * application, where the person can allow them. Always something happens: a row that does nothing is a button that seems broken.
     */
    public void openNotifications() {
        runOnUiThread(() -> {
            android.content.SharedPreferences preferences = getSharedPreferences("notification-consent", MODE_PRIVATE);
            boolean systemWillAsk = android.os.Build.VERSION.SDK_INT >= 33
                    && checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED
                    && !preferences.getBoolean("asked", false);
            if (systemWillAsk) {
                preferences.edit().putBoolean("asked", true).apply();
                requestPermissions(new String[] {android.Manifest.permission.POST_NOTIFICATIONS}, 7004);
                return;
            }
            try {
                startActivity(new Intent(android.provider.Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                        .putExtra(android.provider.Settings.EXTRA_APP_PACKAGE, getPackageName()));
            } catch (RuntimeException unavailable) {
                android.util.Log.w("Peersus", "The settings of the notifications could not be opened");
            }
        });
    }

    @Override public void onWindowFocusChanged(boolean focus) {
        super.onWindowFocusChanged(focus);
        PluginBridge.focus(this, focus);
        if (focus && (TransferService.notificationsRelevant() || TransferNotifications.hasPending())) {
            requestTransferNotifications();
        }
    }

    static native void backgroundTimedOut();

    // The request code of the document picker: the one result this activity waits for.
    private static final int PICK_DOCUMENTS = 7001;
    // The request code of the folder picker.
    private static final int PICK_FOLDER = 7003;
    // The request code of the folder picker for the folder of the files that come in on their own: its answer is kept, and is nobody's Accept.
    private static final int PICK_SAVED_FOLDER = 7004;
    private static final int PICK_PLUGIN = 7005;

    // One picker result is one ordered task, not one task per URI. Provider calls may block:
    // they never run on the rendering/activity thread or contend with received-file publication.
    private static final java.util.concurrent.ThreadPoolExecutor picks =
            new java.util.concurrent.ThreadPoolExecutor(0, 1, 30, java.util.concurrent.TimeUnit.SECONDS,
                    new java.util.concurrent.ArrayBlockingQueue<>(1),
                    task -> new Thread(task, "peersus-picker"));

    /**
     * Opens the system's own folder picker, for the folder a received file is put in. Called from Rust, on any thread.
     * What is chosen is a tree address (text), never a path: the program cannot write through it, only this class can.
     * The answer comes back through {@link #folderChosen}, with null if the person backed out.
     */
    public void chooseFolder() {
        runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            startActivityForResult(intent, PICK_FOLDER);
        });
    }

    /**
     * Opens the system's folder picker for the folder that files which come in on their own are put in. Called from Rust, on any thread.
     * What is chosen is kept (see {@link #savedFolder}) and is not the answer to any one file.
     */
    public void chooseSavedFolder() {
        runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            startActivityForResult(intent, PICK_SAVED_FOLDER);
        });
    }

    /**
     * The folder chosen for the files that come in on their own, as a tree address, or "" when there is none (see {@link SavedFolder}).
     * Called from Rust, on any thread.
     */
    public String savedFolder() {
        return SavedFolder.kept(this);
    }

    /** Lets go of the saved folder: files that come in ask where they go again. Called from Rust, on any thread. */
    public void forgetSavedFolder() {
        SavedFolder.forget(this);
    }

    /** The window is here already, so there is nothing to bring forward: asked the same as of the service, which has to start one. */
    public void bringWindowForward() { }

    /**
     * Puts a file that arrived whole into the folder the person chose (see {@link FolderDelivery}, which the service uses too, so that a file
     * accepted with the window closed ends in the folder). Called from Rust, on any thread.
     */
    public void deliver(final String transfer, final String path, final String name, final String tree,
                        final String[] folders) {
        FolderDelivery.deliver(this, transfer, path, name, tree, folders);
    }

    private long documentGeneration;
    private boolean pluginPickerOpen;

    /** Only the selected archive FD crosses JNI. The owner stages and validates its private copy. */
    public void choosePlugin() {
        runOnUiThread(() -> {
            if (pluginPickerOpen || documentGeneration != 0) {
                android.widget.Toast.makeText(this, "Finish the current file selection first.", android.widget.Toast.LENGTH_LONG).show();
                return;
            }
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            try { pluginPickerOpen = true; startActivityForResult(intent, PICK_PLUGIN); }
            catch (RuntimeException unavailable) {
                pluginPickerOpen = false;
                pluginInstallFailed("The plugin picker could not be opened.");
            }
        });
    }

    public void pluginInstallFailed(String words) {
        runOnUiThread(() -> android.widget.Toast.makeText(this, words, android.widget.Toast.LENGTH_LONG).show());
    }

    private void selectedPlugin(int result, Intent data) {
        if (!pluginPickerOpen) return;
        pluginPickerOpen = false;
        Uri uri = result == RESULT_OK && data != null ? data.getData() : null;
        if (uri == null) return;
        try {
            picks.execute(() -> {
                try (ParcelFileDescriptor descriptor = getContentResolver().openFileDescriptor(uri, "r")) {
                    if (descriptor == null) throw new java.io.IOException();
                    String name = "extension.tar";
                    try (Cursor cursor = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
                        if (cursor != null && cursor.moveToFirst()) name = cursor.getString(0);
                    }
                    pluginChosen(descriptor.detachFd(), name);
                } catch (java.io.IOException | RuntimeException unavailable) {
                    pluginInstallFailed("The selected plugin archive could not be read.");
                }
            });
        } catch (java.util.concurrent.RejectedExecutionException busy) {
            pluginInstallFailed("Another file is being opened; choose the plugin again afterwards.");
        }
    }

    private static native void pluginChosen(int descriptor, String name);

    /**
     * Opens the system's own document picker. Called from Rust, on any thread: the picker is started on the
     * interface thread, which is where an activity may start another. The chosen files come back through
     * {@link #onActivityResult} and are handed to Rust as open descriptors, never as paths, since a picked file
     * has none that the program could read.
     */
    public void chooseDocuments(final boolean several) {
        final long generation = pickerGeneration();
        runOnUiThread(() -> {
            // One system picker may be outstanding. A second request must not relabel its eventual result.
            if (documentGeneration != 0 || pluginPickerOpen) {
                pickFinished(generation);
                return;
            }
            documentGeneration = generation;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, several);
            startActivityForResult(intent, PICK_DOCUMENTS);
        });
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == PICK_PLUGIN) { selectedPlugin(result, data); return; }
        if (request == PICK_SAVED_FOLDER) {
            Uri tree = result == RESULT_OK && data != null ? data.getData() : null;
            if (tree != null) {
                try {
                    // Kept across restarts of the program: a file that arrives with the window closed is put in it without asking.
                    getContentResolver().takePersistableUriPermission(tree,
                            Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
                    SavedFolder.keep(this, tree.toString());
                } catch (SecurityException notPersistable) {
                    // A provider that does not let the grant be kept cannot be a folder that is written later with nobody there: not kept.
                }
            }
            return;
        }
        if (request == PICK_FOLDER) {
            Uri tree = result == RESULT_OK && data != null ? data.getData() : null;
            if (tree != null) {
                try {
                    // Kept across restarts of the program, so that a file that arrives later can still be put in it.
                    getContentResolver().takePersistableUriPermission(tree,
                            Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
                } catch (SecurityException notPersistable) {
                    // The provider does not allow keeping it: the grant of this run still lets the file be put there now.
                }
            }
            // Backing out is an answer too (null): Rust is told, so that it does not wait for a folder that will not come.
            folderChosen(tree == null ? null : tree.toString());
            return;
        }
        if (request != PICK_DOCUMENTS) return;
        final long generation = documentGeneration;
        documentGeneration = 0;
        if (result != RESULT_OK || data == null) {
            // Backing out is an answer too: Rust is told, so that it does not wait for files that will not come.
            pickFinished(generation);
            return;
        }
        List<Uri> chosen = new ArrayList<>();
        if (data.getClipData() != null) {
            for (int i = 0; i < data.getClipData().getItemCount(); i++) chosen.add(data.getClipData().getItemAt(i).getUri());
        } else if (data.getData() != null) {
            chosen.add(data.getData());
        }
        deliverChosen(generation, chosen);
    }

    /**
     * Hands the files of one selection to Rust as open descriptors, under the generation Rust gave it: the complete count first, then each
     * file, then the end. The same for what the system's picker returned and for what another application shared, so that both are held,
     * counted, refused and sent by one set of rules.
     */
    private void deliverChosen(final long generation, final List<Uri> chosen) {
        pickStarted(generation, chosen.size());
        try {
            picks.execute(() -> {
                try {
                    for (Uri uri : chosen) {
                        int state;
                        try {
                            while ((state = pickerState(generation)) == 0) Thread.sleep(20);
                        } catch (InterruptedException interrupted) {
                            Thread.currentThread().interrupt();
                            return;
                        }
                        if (state < 0) return;
                        try {
                            // Query before detaching: even a provider failure cannot strand an owned descriptor.
                            String name = displayName(uri);
                            try (ParcelFileDescriptor descriptor = getContentResolver().openFileDescriptor(uri, "r")) {
                                if (descriptor == null) continue;
                                // Rust owns and closes the descriptor after detachFd.
                                pickedFile(generation, descriptor.detachFd(), name);
                            }
                        } catch (Exception unreadable) {
                            // A revoked/unreadable file does not prevent the remaining choices from arriving.
                        }
                    }
                } finally {
                    // Completion must follow every descriptor handoff on this same worker.
                    pickFinished(generation);
                }
            });
        } catch (java.util.concurrent.RejectedExecutionException busy) {
            // A bounded worker must also complete a refused request, rather than leave the interface waiting.
            pickFinished(generation);
        }
    }

    /** The most files one share is taken for: what one selection may hold, so that a share cannot ask for more than a picker could. */
    private static final int MOST_SHARED = 64;

    @SuppressWarnings("deprecation")
    private static Uri streamOf(Intent intent) {
        // The untyped getter, cast by hand: the typed one exists only from Android 13, and this runs from Android 12. A value that is not an
        // address is a ClassCastException, which the caller treats as a malformed share.
        Object value = intent.getParcelableExtra(Intent.EXTRA_STREAM);
        return value == null ? null : (Uri) value;
    }

    @SuppressWarnings("deprecation")
    private static List<Uri> streamsOf(Intent intent) {
        List<Uri> out = new ArrayList<>();
        ArrayList<android.os.Parcelable> many = intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM);
        if (many != null) for (android.os.Parcelable one : many) if (one != null) out.add((Uri) one);
        return out;
    }

    /**
     * Files another application shared with this one (the share sheet): their addresses are taken from the intent, a selection is opened for
     * them in Rust, and they are delivered as a picker's choice is. Nothing is read from an address here that the sharing application did
     * not grant for this use, and what cannot be opened is counted as refused on the Rust side, never read some other way. A share while
     * a picker is open is turned away, and said so: what the person chose there is theirs.
     */
    private void handleSharedFiles(Intent intent) {
        if (intent == null) return;
        String action = intent.getAction();
        if (!Intent.ACTION_SEND.equals(action) && !Intent.ACTION_SEND_MULTIPLE.equals(action)) return;
        List<Uri> shared = new ArrayList<>();
        try {
            // Text is the clipboard plugin's, through PluginBridge; only what is a file (an address) is handled here. Decided before the
            // intent is touched, so that text that is not ours is left exactly as it came.
            if ("text/plain".equals(intent.getType()) && streamOf(intent) == null && streamsOf(intent).isEmpty()) return;
        } catch (RuntimeException malformed) {
            return;
        }
        try {
            if (Intent.ACTION_SEND.equals(action)) {
                Uri one = streamOf(intent);
                if (one != null) shared.add(one);
            } else {
                shared.addAll(streamsOf(intent));
            }
            if (shared.isEmpty() && intent.getClipData() != null) {
                for (int i = 0; i < intent.getClipData().getItemCount(); i++) {
                    Uri uri = intent.getClipData().getItemAt(i).getUri();
                    if (uri != null) shared.add(uri);
                }
            }
        } catch (RuntimeException malformed) {
            return;
        } finally {
            // Taken once: the window made again (turned, or restored) must not take the same share a second time.
            intent.removeExtra(Intent.EXTRA_STREAM);
            intent.setAction(Intent.ACTION_MAIN);
        }
        if (shared.isEmpty()) return;
        // Only content the sharing application granted: a file:// address would be read with this application's own rights.
        shared.removeIf(uri -> !"content".equals(uri.getScheme()));
        if (shared.isEmpty()) {
            android.widget.Toast.makeText(this, "These files cannot be shared with Peersus.", android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        if (shared.size() > MOST_SHARED) shared = new ArrayList<>(shared.subList(0, MOST_SHARED));
        final List<Uri> taken = shared;
        runOnUiThread(() -> openSharedWhenReady(taken, 0));
    }

    /** How many times, and how far apart, the intake looks again for the program to have started: it is the program that is not there yet when a share starts the application. */
    private static final int SHARE_TRIES = 40;
    private static final long SHARE_EVERY_MS = 100;

    /**
     * Opens the selection for a share. A share can be what starts the application, and then this runs before the program in Rust has made
     * the state that holds the files: that is "not ready" (negative), looked at again a moment later, a few seconds at most, never given up
     * as refused. A refusal (zero) is a picker or an earlier share already open, and is said.
     */
    private void openSharedWhenReady(final List<Uri> taken, final int attempt) {
        if (isFinishing()) return;
        if (documentGeneration != 0 || pluginPickerOpen) {
            android.widget.Toast.makeText(this, "Finish the current file selection first.", android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        long generation = openSharedSelection();
        if (generation < 0) {
            if (attempt >= SHARE_TRIES) {
                android.widget.Toast.makeText(this, "Peersus is not ready yet. Share the files again.", android.widget.Toast.LENGTH_LONG).show();
                return;
            }
            getWindow().getDecorView().postDelayed(() -> openSharedWhenReady(taken, attempt + 1), SHARE_EVERY_MS);
            return;
        }
        if (generation == 0) {
            android.widget.Toast.makeText(this, "Finish the current file selection first.", android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        deliverChosen(generation, taken);
    }

    /** The name the provider gives the file, which is not trusted and is judged on the Rust side. */
    private String displayName(Uri uri) {
        try (Cursor cursor = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                String name = cursor.getString(0);
                if (name != null) return name;
            }
        } catch (Exception unnamed) {
            // No name: Rust gives the file a plain one.
        }
        return "";
    }

    /**
     * Opens a selection in Rust for files that came from outside the window (a share), as the window does when the person asks for the
     * picker. Returns its generation, or 0 when a picker is already open or a share has not been taken yet: the share is then refused.
     */
    private native long openSharedSelection();

    /** Declare the complete selection before its first descriptor is opened. */
    private native void pickStarted(long generation, int count);

    /** One chosen file, as an open descriptor that now belongs to Rust, and its name. */
    private native void pickedFile(long generation, int descriptor, String name);

    private native long pickerGeneration();
    private native int pickerState(long generation);

    /** Completion belongs to the same picker generation as its descriptor handoffs. */
    private native void pickFinished(long generation);

    /** The folder the person chose, as a tree address, or null if they backed out. Rust's. */
    private native void folderChosen(String tree);

    // The camera, made when first asked for. The request code of its permission is the one result this waits for.
    private static final int CAMERA_PERMISSION = 7002;
    private CodeCamera camera;

    /**
     * Open the camera to read the code of another device. Called from Rust, on any thread. If the permission is not
     * held it is asked for, and the camera starts when it is granted; if it is refused nothing starts, and Rust is told
     * with {@link #cameraStopped} so that it does not wait for frames that will not come.
     */
    public void openCamera() {
        runOnUiThread(() -> {
            if (camera == null) camera = new CodeCamera(this);
            if (camera.allowed()) {
                camera.start();
            } else {
                requestPermissions(new String[] {android.Manifest.permission.CAMERA}, CAMERA_PERMISSION);
            }
        });
    }

    /** Close the camera: the person left, or a code was read. Safe to call when it is not open. */
    public void closeCamera() {
        runOnUiThread(() -> {
            if (camera != null) camera.stop();
        });
    }

    /** The back cameras of this phone for Settings, one line each; see {@link CodeCamera#describeBackCameras}. Called from Rust. */
    public String backCameras() {
        return CodeCamera.describeBackCameras(this);
    }


    /**
     * The camera the person chose, by its id; an empty one gives the choice back to the program. If the camera is open it is started again
     * on the new one, so that the circle shows what the choice looks like and the person can compare them. Called from Rust.
     */
    public void chooseCamera(String id) {
        getSharedPreferences(CodeCamera.PREFERENCES, MODE_PRIVATE).edit().putString(CodeCamera.CHOICE, id == null ? "" : id).apply();
        runOnUiThread(() -> {
            if (camera != null && camera.running()) {
                camera.stop();
                camera.start();
            }
        });
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(request, permissions, results);
        if (request == 7004) {
            boolean allowed = results.length > 0 && results[0] == android.content.pm.PackageManager.PERMISSION_GRANTED;
            if (allowed) TransferService.notificationPermissionChanged(this);
            else android.widget.Toast.makeText(this, AppLanguage.context(this).getString(R.string.notifications_disabled), android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        if (request != CAMERA_PERMISSION) return;
        boolean granted = results.length > 0 && results[0] == android.content.pm.PackageManager.PERMISSION_GRANTED;
        if (granted && camera != null) {
            camera.start();
        } else {
            // Refused: said to Rust, which tells the person, instead of a circle that waits for ever.
            cameraStopped();
        }
    }

    @Override
    protected void onPause() {
        PluginBridge.pause(this);
        TransferService.paused();
        TransferNotifications.visible(this, false);
        // A camera must not stay open when the application is not in front: it is closed, and opened again if asked.
        if (camera != null) camera.stop();
        super.onPause();
    }

    @Override protected void onDestroy() {
        ClipboardAccess.detach(this);
        super.onDestroy();
    }

    /** The camera could not be used: the permission was refused. Rust's. */
    private native void cameraStopped();
}
