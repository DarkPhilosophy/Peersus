package dev.peersus.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.os.Handler;
import android.os.Looper;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.text.InputType;
import android.util.AtomicFile;
import android.widget.CheckBox;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.util.Arrays;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/** Credentials are released only after a destination-and-file confirmation, never to a guest. */
final class ExtensionAuthorization {
    private static final String KEY = "peersus-extension-credentials-v1";
    private static final ThreadPoolExecutor WORK = new ThreadPoolExecutor(
            0, 1, 30, TimeUnit.SECONDS, new ArrayBlockingQueue<>(4),
            task -> new Thread(task, "peersus-extension-credential"));
    private final Activity activity;
    private final Handler ui = new Handler(Looper.getMainLooper());
    private AlertDialog dialog;
    private AlertDialog resultDialog;
    private volatile long pending;
    private volatile boolean destroyed;

    ExtensionAuthorization(Activity activity) { this.activity = activity; }

    void request(long request, String caller, String origin, String filename, long bytes) {
        if (bytes == 0 && filename.isEmpty()) {
            configure(request, caller, origin);
            return;
        }
        activity.runOnUiThread(() -> {
            if (request <= 0 || destroyed || activity.isFinishing() || activity.isDestroyed()
                    || pending != 0 || bytes <= 0) {
                authorized(request, null);
                return;
            }
            pending = request;
            LinearLayout content = new LinearLayout(activity);
            content.setOrientation(LinearLayout.VERTICAL);
            int padding = Math.round(20 * activity.getResources().getDisplayMetrics().density);
            content.setPadding(padding, padding, padding, padding);
            TextView details = new TextView(activity);
            details.setText("Extension: " + caller + "\nDestination: " + origin
                    + "\nFile: " + filename + "\nBytes: " + bytes
                    + "\n\nAllow this extension to upload this file to this destination? "
                    + "Use a least-privilege credential, not an owner credential. "
                    + "The credential stays in the application host. "
                    + "The destination receives the file in plaintext; this is not end-to-end encrypted. "
                    + "Anyone with a returned public link may access it. Retention is controlled by the destination.");
            content.addView(details);
            EditText credential = new EditText(activity);
            credential.setHint("Machine credential");
            credential.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
            credential.setImportantForAutofill(android.view.View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);
            credential.setSaveEnabled(false);
            content.addView(credential);
            CheckBox remember = new CheckBox(activity);
            remember.setText("Remember encrypted on this device for this extension and destination");
            content.addView(remember);
            dialog = new AlertDialog.Builder(activity).setTitle("Approve extension upload")
                    .setView(content).setNegativeButton("Cancel", (which, button) -> finish(request, null))
                    .setPositiveButton("Approve", null)
                    .setNeutralButton("Use saved credential", null)
                    .setOnCancelListener(which -> finish(request, null)).create();
            dialog.setOnDismissListener(which -> credential.getText().clear());
            dialog.setOnShowListener(which -> {
                dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener(view -> {
                    int length = credential.length();
                    if (length == 0 || length > 8192) {
                        credential.setError("Enter a valid credential");
                        return;
                    }
                    byte[] secret = new byte[length];
                    for (int index = 0; index < length; index++) {
                        char character = credential.getText().charAt(index);
                        if (character < 33 || character > 126) {
                            Arrays.fill(secret, (byte) 0);
                            credential.setError("The credential contains invalid characters");
                            return;
                        }
                        secret[index] = (byte) character;
                    }
                    credential.getText().clear();
                    submit(request, caller, origin, secret, remember.isChecked(), false);
                });
                dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setOnClickListener(view ->
                        submit(request, caller, origin, null, false, true));
            });
            try { dialog.show(); }
            catch (RuntimeException unavailable) { finish(request, null); }
        });
    }

    /** Configuration never decrypts or returns the saved credential. */
    private void configure(long request, String caller, String origin) {
        activity.runOnUiThread(() -> {
            if (request <= 0 || destroyed || activity.isFinishing() || activity.isDestroyed()
                    || pending != 0) {
                authorized(request, null);
                return;
            }
            pending = request;
            LinearLayout content = new LinearLayout(activity);
            content.setOrientation(LinearLayout.VERTICAL);
            int padding = Math.round(20 * activity.getResources().getDisplayMetrics().density);
            content.setPadding(padding, padding, padding, padding);
            TextView status = new TextView(activity);
            String scope = "Extension: " + caller + "\nDestination: " + origin + "\n\n";
            try {
                status.setText(scope + (record(caller, origin).isFile()
                        ? "API key saved, encrypted on this device." : "No API key saved."));
            } catch (Exception unavailable) {
                status.setText(scope + "Secure storage unavailable.");
            }
            content.addView(status);
            EditText credential = new EditText(activity);
            credential.setHint("API key — least privilege, not an owner credential");
            credential.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
            credential.setImportantForAutofill(android.view.View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);
            credential.setSaveEnabled(false);
            content.addView(credential);
            dialog = new AlertDialog.Builder(activity).setTitle("API key")
                    .setView(content)
                    .setPositiveButton("Save", null)
                    .setNeutralButton("Remove saved key", null)
                    .setNegativeButton("Cancel", (which, button) -> finish(request, null))
                    .setOnCancelListener(which -> finish(request, null)).create();
            dialog.setOnDismissListener(which -> credential.getText().clear());
            dialog.setOnShowListener(which -> {
                dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener(view -> {
                    int length = credential.length();
                    if (length == 0 || length > 8192) {
                        credential.setError("Enter a valid API key");
                        return;
                    }
                    byte[] secret = new byte[length];
                    for (int index = 0; index < length; index++) {
                        char character = credential.getText().charAt(index);
                        if (character < 33 || character > 126) {
                            Arrays.fill(secret, (byte) 0);
                            credential.setError("The API key contains invalid characters");
                            return;
                        }
                        secret[index] = (byte) character;
                    }
                    credential.getText().clear();
                    configureWrite(request, caller, origin, secret, status, scope);
                });
                dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setOnClickListener(view ->
                        configureWrite(request, caller, origin, null, status, scope));
            });
            try { dialog.show(); }
            catch (RuntimeException unavailable) { finish(request, null); }
        });
    }

    private void configureWrite(long request, String caller, String origin, byte[] secret,
                                TextView status, String scope) {
        if (pending != request || dialog == null) {
            if (secret != null) Arrays.fill(secret, (byte) 0);
            return;
        }
        dialog.getButton(AlertDialog.BUTTON_POSITIVE).setEnabled(false);
        dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setEnabled(false);
        try {
            WORK.execute(() -> {
                String result;
                boolean succeeded;
                try {
                    if (pending != request || destroyed) return;
                    if (secret == null) {
                        File saved = record(caller, origin);
                        if (saved.exists() && !saved.delete()) throw new java.io.IOException();
                        result = "Saved API key removed.";
                    } else {
                        write(caller, origin, secret);
                        result = "API key saved, encrypted on this device.";
                    }
                    succeeded = true;
                } catch (Exception unavailable) {
                    result = "Secure storage could not be updated.";
                    succeeded = false;
                } finally {
                    if (secret != null) Arrays.fill(secret, (byte) 0);
                }
                final String message = result;
                final boolean completed = succeeded;
                ui.post(() -> {
                    if (pending == request && dialog != null) {
                        if (completed) {
                            Toast.makeText(activity, message, Toast.LENGTH_LONG).show();
                            finish(request, "configured".getBytes(StandardCharsets.US_ASCII));
                            return;
                        }
                        status.setText(scope + message);
                        dialog.getButton(AlertDialog.BUTTON_POSITIVE).setEnabled(true);
                        dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setEnabled(true);
                    }
                });
            });
        } catch (java.util.concurrent.RejectedExecutionException busy) {
            if (secret != null) Arrays.fill(secret, (byte) 0);
            status.setText(scope + "Secure storage is busy.");
            dialog.getButton(AlertDialog.BUTTON_POSITIVE).setEnabled(true);
            dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setEnabled(true);
        }
    }

    private void submit(long request, String caller, String origin, byte[] supplied,
                        boolean remember, boolean saved) {
        if (pending != request || dialog == null) {
            if (supplied != null) Arrays.fill(supplied, (byte) 0);
            return;
        }
        dialog.getButton(AlertDialog.BUTTON_POSITIVE).setEnabled(false);
        dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setEnabled(false);
        try {
            WORK.execute(() -> {
                byte[] secret = supplied;
                boolean succeeded = false;
                try {
                    if (pending != request || destroyed) return;
                    if (saved) secret = read(caller, origin);
                    if (secret == null || secret.length == 0 || secret.length > 8192) {
                        throw new IllegalArgumentException();
                    }
                    for (byte value : secret) if (value < 33 || value > 126) throw new IllegalArgumentException();
                    if (remember) write(caller, origin, secret);
                    final byte[] result = secret;
                    succeeded = ui.post(() -> {
                        try { finish(request, result); }
                        finally { Arrays.fill(result, (byte) 0); }
                    });
                } catch (Exception unavailable) {
                    // No credential, endpoint, filename or exception reaches logs.
                    ui.post(() -> {
                        if (pending == request && dialog != null) {
                            dialog.getButton(AlertDialog.BUTTON_POSITIVE).setEnabled(true);
                            dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setEnabled(true);
                            dialog.setTitle("Credential unavailable — enter a new credential");
                        }
                    });
                } finally {
                    if (!succeeded && secret != null) Arrays.fill(secret, (byte) 0);
                }
            });
        } catch (java.util.concurrent.RejectedExecutionException busy) {
            if (supplied != null) Arrays.fill(supplied, (byte) 0);
            finish(request, null);
        }
    }

    void cancel(long request) {
        activity.runOnUiThread(() -> {
            if (pending == request) finish(request, null);
        });
    }

    void destroy() {
        destroyed = true;
        if (pending != 0) finish(pending, null);
        if (resultDialog != null) {
            try { resultDialog.dismiss(); }
            catch (RuntimeException unavailable) { /* Activity already gone. */ }
            resultDialog = null;
        }
    }

    private void finish(long request, byte[] secret) {
        if (pending != request) return;
        pending = 0;
        AlertDialog current = dialog;
        dialog = null;
        try { if (current != null) current.dismiss(); }
        catch (RuntimeException unavailable) { /* The activity may already be gone. */ }
        authorized(request, secret);
    }

    void present(String caller, String link) {
        activity.runOnUiThread(() -> {
            if (destroyed || activity.isFinishing() || activity.isDestroyed()) return;
            if (resultDialog != null) resultDialog.dismiss();
            TextView content = new TextView(activity);
            int padding = Math.round(20 * activity.getResources().getDisplayMetrics().density);
            content.setPadding(padding, padding, padding, padding);
            content.setText("Extension: " + caller + "\n\n" + link
                    + "\n\nSelect the link to copy it. Anyone with this public link may access the uploaded file.");
            content.setTextIsSelectable(true);
            resultDialog = new AlertDialog.Builder(activity).setTitle("Extension result")
                    .setView(content).setPositiveButton("Close", null).create();
            try { resultDialog.show(); }
            catch (RuntimeException unavailable) { resultDialog = null; }
        });
    }

    private File record(String caller, String origin) throws Exception {
        byte[] scope = (caller + "\n" + origin).getBytes(StandardCharsets.UTF_8);
        byte[] digest = MessageDigest.getInstance("SHA-256").digest(scope);
        StringBuilder name = new StringBuilder();
        for (byte value : digest) name.append(String.format(java.util.Locale.ROOT, "%02x", value & 255));
        File directory = new File(activity.getNoBackupFilesDir(), "extension-credentials");
        if (!directory.isDirectory() && !directory.mkdir()) throw new java.io.IOException();
        return new File(directory, name.toString());
    }

    private static synchronized SecretKey key(boolean create) throws Exception {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        if (store.containsAlias(KEY)) return (SecretKey) store.getKey(KEY, null);
        if (!create) throw new java.security.KeyStoreException();
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        generator.init(new KeyGenParameterSpec.Builder(KEY,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build());
        return generator.generateKey();
    }

    private void write(String caller, String origin, byte[] secret) throws Exception {
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.ENCRYPT_MODE, key(true));
        cipher.updateAAD((caller + "\n" + origin).getBytes(StandardCharsets.UTF_8));
        byte[] encrypted = cipher.doFinal(secret);
        AtomicFile file = new AtomicFile(record(caller, origin));
        FileOutputStream stream = file.startWrite();
        try {
            DataOutputStream output = new DataOutputStream(stream);
            output.writeInt(cipher.getIV().length);
            output.write(cipher.getIV());
            output.writeInt(encrypted.length);
            output.write(encrypted);
            output.flush();
            file.finishWrite(stream);
        } catch (Exception error) {
            file.failWrite(stream);
            throw error;
        }
    }

    private byte[] read(String caller, String origin) throws Exception {
        AtomicFile file = new AtomicFile(record(caller, origin));
        try (FileInputStream stream = file.openRead(); DataInputStream input = new DataInputStream(stream)) {
            int ivLength = input.readInt();
            if (ivLength != 12) throw new java.io.IOException();
            byte[] iv = new byte[ivLength];
            input.readFully(iv);
            int length = input.readInt();
            if (length < 17 || length > 8208) throw new java.io.IOException();
            byte[] encrypted = new byte[length];
            input.readFully(encrypted);
            if (input.read() != -1) throw new java.io.IOException();
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.DECRYPT_MODE, key(false), new GCMParameterSpec(128, iv));
            cipher.updateAAD((caller + "\n" + origin).getBytes(StandardCharsets.UTF_8));
            return cipher.doFinal(encrypted);
        }
    }

    private static native void authorized(long request, byte[] credential);
}
