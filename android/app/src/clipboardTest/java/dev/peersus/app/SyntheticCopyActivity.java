package dev.peersus.app;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.os.Bundle;
import android.widget.TextView;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import org.json.JSONObject;

/** Test-APK activity: another UID copies fixed synthetic text and observes only matching synthetic receipts. */
public final class SyntheticCopyActivity extends Activity {
    private String nonce;
    private int revision;
    private boolean copied;
    private File acknowledgement;
    private ClipboardManager manager;
    private ClipboardManager.OnPrimaryClipChangedListener listener;
    private int receivedRevision = -1;

    static String text(int revision) {
        if (revision < 0 || revision > 16) throw new IllegalArgumentException("synthetic revision out of range");
        return revision == 0 ? "clipboard-verification-only" : "clipboard-verification-only-" + revision;
    }

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        nonce = getIntent().getStringExtra("validation_nonce");
        revision = getIntent().getIntExtra("revision", -1);
        if (nonce == null || !nonce.matches("[a-f0-9]{16}") || revision < 0 || revision > 16) { finish(); return; }
        acknowledgement = new File(getFilesDir(), "clipboard-source-" + nonce + ".json");
        if (acknowledgement.exists()) { acknowledgement = null; finish(); return; }
        TextView view = new TextView(this);
        view.setText("Synthetic clipboard source\n\n" + text(revision));
        view.setTextSize(20);
        view.setPadding(32, 48, 32, 32);
        setContentView(view);
    }

    @Override public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        if (!focused || copied || acknowledgement == null) return;
        copied = true;
        try {
            manager = (ClipboardManager)getSystemService(CLIPBOARD_SERVICE);
            manager.setPrimaryClip(ClipData.newPlainText("synthetic-external-copy", text(revision)));
            writeAcknowledgement();
            listener = () -> {
                if (!hasWindowFocus()) return;
                ClipData clip = manager.getPrimaryClip();
                if (clip == null || clip.getItemCount() != 1) return;
                CharSequence label = clip.getDescription().getLabel();
                if (!"Peersus".contentEquals(label == null ? "" : label)) return;
                CharSequence value = clip.getItemAt(0).getText();
                for (int candidate = 0; candidate <= 16; candidate++) {
                    if (text(candidate).contentEquals(value == null ? "" : value)) {
                        receivedRevision = candidate;
                        try { writeAcknowledgement(); } catch (Exception failed) { finish(); }
                        break;
                    }
                }
            };
            manager.addPrimaryClipChangedListener(listener);
        } catch (Exception failed) { finish(); }
    }

    private void writeAcknowledgement() throws Exception {
        File temporary = new File(acknowledgement.getParentFile(), acknowledgement.getName() + ".tmp");
        Files.write(temporary.toPath(), new JSONObject().put("revision", revision)
            .put("uid", android.os.Process.myUid()).put("received_revision", receivedRevision)
            .toString().getBytes(StandardCharsets.UTF_8));
        Files.move(temporary.toPath(), acknowledgement.toPath(), java.nio.file.StandardCopyOption.REPLACE_EXISTING);
    }

    @Override public void onDestroy() {
        if (listener != null) manager.removePrimaryClipChangedListener(listener);
        if (acknowledgement != null) new File(acknowledgement.getParentFile(), acknowledgement.getName() + ".tmp").delete();
        if (acknowledgement != null) acknowledgement.delete();
        super.onDestroy();
    }
}
