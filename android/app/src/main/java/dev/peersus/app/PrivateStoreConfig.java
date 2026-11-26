package dev.peersus.app;

import android.content.Context;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.AtomicFile;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import java.util.Arrays;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;
import org.json.JSONObject;

/** Private debug builds only. No plaintext fallback and no credentials in preferences or logs. */
final class PrivateStoreConfig {
    private static final String ALIAS = "peersus.private-development-catalog";
    private static final int MAX = 16384;

    private static AtomicFile record(Context context) {
        return new AtomicFile(new File(context.getNoBackupFilesDir(), "private-development-catalog.enc"));
    }
    private static SecretKey key(boolean create) throws Exception {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        if (store.containsAlias(ALIAS)) return (SecretKey) store.getKey(ALIAS, null);
        if (!create) throw new IOException();
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        generator.init(new KeyGenParameterSpec.Builder(ALIAS,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build());
        return generator.generateKey();
    }
    static synchronized byte[] load(Context context) throws IOException {
        if (!BuildConfig.PRIVATE_STORE) throw new IOException("Private catalogs are not enabled");
        AtomicFile file = record(context);
        if (!file.getBaseFile().exists() && !new File(file.getBaseFile() + ".bak").exists()) return new byte[0];
        byte[] encrypted = null;
        try (var input = file.openRead()) {
            encrypted = input.readNBytes(MAX + 29);
            if (encrypted.length < 29 || encrypted.length > MAX + 28 || input.read() != -1) throw new IOException();
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.DECRYPT_MODE, key(false), new GCMParameterSpec(128, encrypted, 0, 12));
            return cipher.doFinal(encrypted, 12, encrypted.length - 12);
        } catch (Exception refused) {
            throw new IOException("Cannot read private catalog configuration");
        } finally {
            if (encrypted != null) Arrays.fill(encrypted, (byte) 0);
        }
    }
    static synchronized boolean save(Context context, byte[] source) {
        if (!BuildConfig.PRIVATE_STORE || source == null) {
            if (source != null) Arrays.fill(source, (byte) 0);
            return false;
        }
        AtomicFile file = record(context);
        FileOutputStream output = null;
        byte[] encrypted = null, readBack = null;
        try {
            if (source.length == 0 || source.length > MAX) return false;
            JSONObject fields = new JSONObject(new String(source, StandardCharsets.UTF_8));
            // Validate the exact destination before replacing any saved credential.
            StoreDownload.githubPrivate(fields.getString("repository"), fields.getString("directory"),
                    fields.getString("revision"), fields.getString("token"));
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.ENCRYPT_MODE, key(true));
            encrypted = cipher.doFinal(source);
            output = file.startWrite();
            output.write(cipher.getIV());
            output.write(encrypted);
            file.finishWrite(output);
            output = null;
            readBack = load(context);
            return Arrays.equals(source, readBack);
        } catch (Exception refused) {
            if (output != null) file.failWrite(output);
            return false;
        } finally {
            Arrays.fill(source, (byte) 0);
            if (encrypted != null) Arrays.fill(encrypted, (byte) 0);
            if (readBack != null) Arrays.fill(readBack, (byte) 0);
        }
    }
    static StoreDownload downloader(Context context) throws IOException {
        if (!BuildConfig.PRIVATE_STORE) return StoreDownload.production();
        byte[] source = load(context);
        try {
            if (source.length == 0) throw new IOException();
            JSONObject fields = new JSONObject(new String(source, StandardCharsets.UTF_8));
            return StoreDownload.githubPrivate(fields.getString("repository"), fields.getString("directory"),
                    fields.getString("revision"), fields.getString("token"));
        } catch (Exception refused) {
            throw new IOException("Private catalog configuration is not usable");
        } finally {
            Arrays.fill(source, (byte) 0);
        }
    }
}
