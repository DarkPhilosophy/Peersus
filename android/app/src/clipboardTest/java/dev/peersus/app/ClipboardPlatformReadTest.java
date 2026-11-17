package dev.peersus.app;

import android.app.AppOpsManager;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.os.SystemClock;
import org.junit.Test;
import static org.junit.Assert.*;
import static androidx.test.platform.app.InstrumentationRegistry.getInstrumentation;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import org.json.JSONObject;

/** Opt-in physical probe: the raw Android API, not ClipboardAccess's foreground guard. */
public final class ClipboardPlatformReadTest {
    @Test
    public void testAllowedAppOpDoesNotAuthorizeBackgroundReads() throws Exception {
        android.os.Bundle args = androidx.test.platform.app.InstrumentationRegistry.getArguments();
        assertEquals("synthetic clipboard writes require explicit opt-in", "true", args.getString("synthetic_clipboard"));
        String nonce = args.getString("validation_nonce");
        assertNotNull(nonce);
        assertTrue(nonce.matches("[a-f0-9]{16}"));
        Context owner = getInstrumentation().getTargetContext();
        String sourcePackage = getInstrumentation().getContext().getPackageName();
        AppOpsManager operations = owner.getSystemService(AppOpsManager.class);
        assertEquals("probe requires an already-allowed app-op; it never grants one", AppOpsManager.MODE_ALLOWED,
            rawReadMode(operations, owner));
        String acknowledgement = "files/clipboard-source-" + nonce + ".json";
        assertFalse("reserved test acknowledgement already exists", shell("run-as " + sourcePackage + " test -e " + acknowledgement + " && echo occupied").contains("occupied"));
        ClipboardManager manager = owner.getSystemService(ClipboardManager.class);
        try {
            String launch = shell("am start -W -n " + sourcePackage + "/dev.peersus.app.SyntheticCopyActivity"
                + " --ei revision 15 --es validation_nonce " + nonce);
            assertTrue("external synthetic copy activity failed to launch", launch.contains("Status: ok"));
            JSONObject copied = null;
            long deadline = SystemClock.elapsedRealtime() + 10000;
            do {
                String status = shell("run-as " + sourcePackage + " cat " + acknowledgement).trim();
                if (status.startsWith("{")) { copied = new JSONObject(status); break; }
                Thread.sleep(50);
            } while (SystemClock.elapsedRealtime() < deadline);
            assertNotNull("external copy did not acknowledge real window focus and write", copied);
            assertEquals(15, copied.getInt("revision"));
            assertTrue("copy must be performed by another app UID", copied.getInt("uid") != android.os.Process.myUid());
            // No Activity belonging to the target app is started. Go straight to the framework:
            // a null result here cannot be caused by ClipboardAccess.eligible().
            final ClipData[] clip = {null};
            final android.content.ClipDescription[] description = {null};
            getInstrumentation().runOnMainSync(() -> {
                clip[0] = manager.getPrimaryClip();
                description[0] = manager.getPrimaryClipDescription();
            });
            assertNull("this Android build permits background reads; revise the capability decision", clip[0]);
            assertNull("this Android build exposes background clipboard metadata", description[0]);
            assertEquals("the app-op must remain allowed after the platform refusal", AppOpsManager.MODE_ALLOWED,
                rawReadMode(operations, owner));
        } finally {
            assertTrue(getInstrumentation().getUiAutomation().performGlobalAction(
                android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_HOME));
            long cleanupDeadline = SystemClock.elapsedRealtime() + 5000;
            while (shell("run-as " + sourcePackage + " test -e " + acknowledgement + " && echo occupied").contains("occupied")) {
                if (SystemClock.elapsedRealtime() >= cleanupDeadline) fail("synthetic Activity did not clean its own acknowledgement");
                Thread.sleep(50);
            }
        }
    }

    // This opt-in probe observes the raw configured mode, without resolving foreground-only modes.
    @SuppressWarnings("deprecation")
    private static int rawReadMode(AppOpsManager operations, Context owner) {
        return operations.unsafeCheckOpNoThrow("android:read_clipboard", android.os.Process.myUid(), owner.getPackageName());
    }

    private String shell(String command) throws Exception {
        try (InputStream input = new android.os.ParcelFileDescriptor.AutoCloseInputStream(
                getInstrumentation().getUiAutomation().executeShellCommand(command));
                java.io.ByteArrayOutputStream output = new java.io.ByteArrayOutputStream()) {
            byte[] buffer = new byte[1024];
            int count;
            while ((count = input.read(buffer)) != -1) {
                assertTrue("test command output exceeded limit", output.size() + count <= 32768);
                output.write(buffer, 0, count);
            }
            return new String(output.toByteArray(), StandardCharsets.UTF_8);
        }
    }
}
