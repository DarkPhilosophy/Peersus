package dev.peersus.app;

import android.app.Activity;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.AtomicFile;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.util.Arrays;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/** Storage only. Consent and configuration are rendered by the shared application UI. */
final class ExtensionAuthorization {
    private static final String KEY = "peersus-extension-credentials-v1";
    private final Activity activity;

    ExtensionAuthorization(Activity activity) { this.activity = activity; }

    synchronized byte[] access(String caller, String origin, int operation, byte[] supplied)
            throws Exception {
        try {
            if (operation == 0) {
                byte[] result = read(caller, origin);
                boolean valid = result.length > 0 && result.length <= 8192;
                for (byte value : result) valid &= value >= 33 && value <= 126;
                if (!valid) {
                    Arrays.fill(result, (byte) 0);
                    throw new java.io.IOException();
                }
                return result;
            }
            if (operation == 1) {
                if (supplied == null || supplied.length == 0 || supplied.length > 8192)
                    throw new IllegalArgumentException();
                for (byte value : supplied)
                    if (value < 33 || value > 126) throw new IllegalArgumentException();
                write(caller, origin, supplied);
            } else if (operation == 2) {
                File saved = record(caller, origin);
                if (saved.exists() && !saved.delete()) throw new java.io.IOException();
            } else {
                throw new IllegalArgumentException();
            }
            return new byte[0];
        } finally {
            if (supplied != null) Arrays.fill(supplied, (byte) 0);
        }
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

}
