package dev.peersus.app;

import android.content.Context;
import android.content.UriPermission;

/**
 * The folder that files which come in on their own are put in, as the tree address the system's picker gave. Kept in the program's own
 * preferences and read from any context: the activity that shows the picker, the service that lives with the window closed, and the panel
 * over other applications all ask here, so that they cannot disagree about whether there is a folder.
 */
final class SavedFolder {
    static final String PREFERENCES = "save-folder";
    static final String KEY = "tree";

    private SavedFolder() { }

    /**
     * The tree address, or "" when there is none. A folder whose permission the system no longer holds (it was removed, or the grant was
     * taken back) is as good as none: it is let go of here, so that a file is not accepted into a place that cannot be written and then lost.
     */
    static String kept(Context context) {
        String tree = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(KEY, "");
        if (tree.isEmpty()) return "";
        for (UriPermission held : context.getContentResolver().getPersistedUriPermissions()) {
            if (held.getUri().toString().equals(tree) && held.isWritePermission()) return tree;
        }
        forget(context);
        return "";
    }

    /** Keep `tree` as the folder. */
    static void keep(Context context, String tree) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putString(KEY, tree).apply();
    }

    /** Let go of the folder: files that come in ask where they go again. */
    static void forget(Context context) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().remove(KEY).apply();
    }
}
