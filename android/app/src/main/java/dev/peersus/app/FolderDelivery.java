package dev.peersus.app;

import android.content.Context;
import android.content.ContentResolver;
import android.database.Cursor;
import android.net.Uri;
import android.provider.DocumentsContract;

/**
 * Puts a file that arrived whole into the folder the person chose: copied into it, and only when the copy is complete is the file the program
 * staged removed. The folder is a tree address the system gave, which only the system's own calls can write through, so this is Java's.
 *
 * It takes a {@link Context} and not the window, so that the service can do it too: a file accepted with the window closed (saving on their own
 * is on and a folder is chosen) must end in the folder, and not wait in the staging for the window to be opened.
 */
final class FolderDelivery {
    static { System.loadLibrary("peersus_android"); }

    /** A file was put in the chosen folder (true) or could not be (false, and the staged file is kept). Rust's. */
    static native void delivered(String transfer, boolean done);

    // A single writer also serializes directory creation for files sharing a parent.
    // Saturation is reported as a failed handoff; the staged original is retained.
    private static final java.util.concurrent.ThreadPoolExecutor deliveries =
            new java.util.concurrent.ThreadPoolExecutor(1, 1, 30, java.util.concurrent.TimeUnit.SECONDS,
                    new java.util.concurrent.ArrayBlockingQueue<>(16),
                    task -> new Thread(task, "peersus-deliver"));

    private FolderDelivery() { }

    /**
     * Copies the staged file into the folder on a thread of its own, so that whoever asked is not held while a large file is written.
     * {@link #delivered} says how it went. Safe from any thread.
     */
    static void deliver(Context context, final String transfer, final String path, final String name, final String tree,
                        final String[] folders) {
        final ContentResolver resolver = context.getApplicationContext().getContentResolver();
        Runnable copy = () -> {
            boolean done = false;
            Uri target = null;
            try {
                Uri root = Uri.parse(tree);
                Uri parent = DocumentsContract.buildDocumentUriUsingTree(root, DocumentsContract.getTreeDocumentId(root));
                if (!isOneName(name) || folders == null || folders.length > 64) throw new java.io.IOException("invalid destination");
                for (String folder : folders) parent = ensureDirectory(resolver, root, parent, folder);
                target = DocumentsContract.createDocument(resolver, parent, "application/octet-stream", name);
                if (target != null) {
                    // The copy counts as complete only once both ends are CLOSED without an error: a provider can fail its
                    // write at close, after every byte was accepted, and a file that was not really written is not delivered.
                    copyInto(resolver, path, target);
                    done = true;
                }
            } catch (Exception failed) {
                // Said as not delivered: the staged file is kept, so that nothing the person was sent is lost.
            }
            if (done) {
                new java.io.File(path).delete();
            } else if (target != null) {
                // A copy that did not finish leaves a partial file in the person's folder: it is removed, not kept.
                try {
                    DocumentsContract.deleteDocument(resolver, target);
                } catch (Exception stays) {
                    // Best effort: the provider may refuse; the staged file is still kept either way.
                }
            }
            delivered(transfer, done);
        };
        try {
            deliveries.execute(copy);
        } catch (java.util.concurrent.RejectedExecutionException saturated) {
            delivered(transfer, false);
        }
    }

    private static boolean isOneName(String name) {
        return name != null && !name.isEmpty() && !name.equals(".") && !name.equals("..")
                && name.indexOf('/') < 0 && name.indexOf('\\') < 0 && name.indexOf('\0') < 0;
    }

    /** Reuse only an actual directory with the exact name, never a file or an ambiguous provider result. */
    private static Uri ensureDirectory(ContentResolver resolver, Uri tree, Uri parent, String name) throws java.io.IOException {
        if (!isOneName(name)) throw new java.io.IOException("invalid directory name");
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, DocumentsContract.getDocumentId(parent));
        String[] columns = {
                DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                DocumentsContract.Document.COLUMN_MIME_TYPE
        };
        Uri found = null;
        try (Cursor cursor = resolver.query(children, columns, null, null, null)) {
            if (cursor == null) throw new java.io.IOException("directory cannot be inspected");
            while (cursor.moveToNext()) {
                if (!name.equals(cursor.getString(1))) continue;
                if (found != null || !DocumentsContract.Document.MIME_TYPE_DIR.equals(cursor.getString(2)))
                    throw new java.io.IOException("directory name is occupied");
                found = DocumentsContract.buildDocumentUriUsingTree(tree, cursor.getString(0));
            }
        }
        if (found != null) return found;
        Uri created = DocumentsContract.createDocument(resolver, parent, DocumentsContract.Document.MIME_TYPE_DIR, name);
        if (created == null) throw new java.io.IOException("directory could not be created");
        return created;
    }

    /** Copy the file at {@code path} into the document {@code target}. Returns only if it was written and closed without error. */
    private static void copyInto(ContentResolver resolver, String path, Uri target) throws java.io.IOException {
        try (java.io.InputStream in = new java.io.FileInputStream(path);
             java.io.OutputStream out = resolver.openOutputStream(target, "w")) {
            if (out == null) throw new java.io.IOException("the provider gave no stream to write");
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = in.read(buffer)) > 0) out.write(buffer, 0, read);
            out.flush();
        }
    }
}
