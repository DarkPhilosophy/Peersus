package dev.peersus.app;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import androidx.test.core.app.ActivityScenario;
import org.junit.After;
import org.junit.Test;
import static org.junit.Assert.*;
import static androidx.test.platform.app.InstrumentationRegistry.getInstrumentation;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import org.json.JSONObject;

/** Explicit opt-in: overwrites the real clipboard with synthetic text, never reads its prior contents. */
public final class ClipboardSyncTest {
    private ActivityScenario<MainActivity> scenario;
    @After public void closeScenario() {
        if (scenario != null) scenario.close();
    }


    @Test
    public void testRealClipboardLifecycleWithIsolatedPairedOwner() throws Exception {
        android.os.Bundle args = androidx.test.platform.app.InstrumentationRegistry.getArguments();
        assertEquals("real clipboard writes require explicit test opt-in", "true", args.getString("synthetic_clipboard"));
        String nonce = args.getString("validation_nonce");
        assertNotNull(nonce);
        assertTrue(nonce.matches("[a-f0-9]{16}"));
        String expectedPeer = args.getString("expected_peer");
        assertNotNull("an exact paired target is required before any clipboard mutation", expectedPeer);
        assertTrue(expectedPeer.matches("[a-f0-9]{64}"));
        scenario = ActivityScenario.launch(new Intent(Intent.ACTION_MAIN).setClass(getInstrumentation().getTargetContext(), MainActivity.class)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
        java.util.concurrent.atomic.AtomicReference<MainActivity> launchedActivity = new java.util.concurrent.atomic.AtomicReference<>();
        scenario.onActivity(launchedActivity::set);
        MainActivity activity = launchedActivity.get();
        File directory = new File(activity.getFilesDir(), "clipboard-validation-" + nonce);
        assertTrue("reserved validation directory already exists", directory.mkdir());
        File command = new File(directory, "command.json");
        File status = new File(directory, "status.json");
        long deadline = android.os.SystemClock.elapsedRealtime() + 240000;
        long sequence = 0;
        boolean syntheticWritten = false;
        int copies = 0, received = 0;
        boolean backgrounded = false, foregrounded = false;
        JSONObject diagnostic = null;
        try {
            awaitFocus(activity, true);
            awaitPairedTarget(activity, expectedPeer);
            status(status, 0, true);
            while (android.os.SystemClock.elapsedRealtime() < deadline) {
                if (!command.isFile()) { Thread.sleep(50); continue; }
                JSONObject request = new JSONObject(new String(Files.readAllBytes(command.toPath()), StandardCharsets.UTF_8));
                long next = request.getLong("sequence");
                if (next <= sequence) { Thread.sleep(50); continue; }
                assertEquals(sequence + 1, next);
                String action = request.getString("action");
                int revision = request.optInt("revision", 0);
                assertTrue("only bounded synthetic revisions are allowed", revision >= 0 && revision <= 16);
                String text = SyntheticCopyActivity.text(revision);
                if ("finish".equals(action)) {
                    assertTrue("no real synthetic copies were exercised", copies >= 2);
                    assertTrue("no remote clipboard writes were verified", received >= 2);
                    assertTrue("background/resume lifecycle was not exercised", backgrounded && foregrounded);
                    status(status, next, true);
                    return;
                }
                if ("copy".equals(action)) {
                    getInstrumentation().runOnMainSync(() -> ((ClipboardManager)activity.getSystemService(android.content.Context.CLIPBOARD_SERVICE))
                        .setPrimaryClip(ClipData.newPlainText("synthetic-clipboard-validation", text)));
                    syntheticWritten = true;
                    copies++;
                } else if ("external-copy".equals(action)) {
                    String testPackage = getInstrumentation().getContext().getPackageName();
                    String launched = DeviceCommand.output("am start -W -n " + testPackage + "/dev.peersus.app.SyntheticCopyActivity"
                        + " --ei revision " + revision + " --es validation_nonce " + nonce);
                    assertTrue("synthetic external activity did not start", launched.contains("Status: ok"));
                    awaitFocus(activity, false);
                    long copiedUntil = android.os.SystemClock.elapsedRealtime() + 5000;
                    JSONObject copied = null;
                    do {
                        String acknowledgement = DeviceCommand.output("run-as " + testPackage + " cat files/clipboard-source-" + nonce + ".json").trim();
                        if (acknowledgement.startsWith("{")) { copied = new JSONObject(acknowledgement); break; }
                        Thread.sleep(50);
                    } while (android.os.SystemClock.elapsedRealtime() < copiedUntil);
                    assertNotNull("external copy was not acknowledged", copied);
                    assertEquals(revision, copied.getInt("revision"));
                    assertTrue("copy must come from another app UID", copied.getInt("uid") != android.os.Process.myUid());
                    syntheticWritten = true;
                    copies++;
                    backgrounded = true;
                } else if ("external-expect".equals(action)) {
                    assertTrue("never observe preexisting clipboard contents", syntheticWritten);
                    awaitFocus(activity, false);
                    String testPackage = getInstrumentation().getContext().getPackageName();
                    long receivedUntil = android.os.SystemClock.elapsedRealtime() + 10000;
                    boolean matches = false;
                    do {
                        String acknowledgement = DeviceCommand.output("run-as " + testPackage + " cat files/clipboard-source-" + nonce + ".json").trim();
                        if (acknowledgement.startsWith("{")) {
                            JSONObject observed = new JSONObject(acknowledgement);
                            matches = observed.optInt("received_revision", -1) == revision;
                        }
                        if (matches) break;
                        Thread.sleep(50);
                    } while (android.os.SystemClock.elapsedRealtime() < receivedUntil);
                    assertTrue("remote write did not reach the OS clipboard while the target app was backgrounded", matches);
                    received++;
                } else if ("expect".equals(action)) {
                    assertTrue("never read preexisting clipboard contents", syntheticWritten);
                    awaitFocus(activity, true);
                    final boolean[] matches = {false};
                    long receivedUntil = android.os.SystemClock.elapsedRealtime() + 10000;
                    do {
                        getInstrumentation().runOnMainSync(() -> {
                            ClipData clip = ((ClipboardManager)activity.getSystemService(android.content.Context.CLIPBOARD_SERVICE)).getPrimaryClip();
                            if (clip == null || clip.getItemCount() != 1) return;
                            CharSequence label = clip.getDescription().getLabel();
                            if (!"Peersus".contentEquals(label == null ? "" : label)) return;
                            matches[0] = text.contentEquals(clip.getItemAt(0).getText() == null ? "" : clip.getItemAt(0).getText());
                        });
                        if (matches[0]) break;
                        Thread.sleep(50);
                    } while (android.os.SystemClock.elapsedRealtime() < receivedUntil);
                    assertTrue("synthetic clipboard did not arrive", matches[0]);
                    received++;
                } else if ("diagnose".equals(action)) {
                    assertTrue("never inspect preexisting clipboard contents", syntheticWritten);
                    awaitFocus(activity, true);
                    JSONObject observed = new JSONObject();
                    getInstrumentation().runOnMainSync(() -> {
                        try {
                            for (String field : new String[]{"resumed", "focused", "observing", "listening", "dirty"}) {
                                java.lang.reflect.Field value = ClipboardAccess.class.getDeclaredField(field);
                                value.setAccessible(true);
                                observed.put(field, value.getBoolean(null));
                            }
                            java.lang.reflect.Field manager = ClipboardAccess.class.getDeclaredField("manager");
                            manager.setAccessible(true);
                            observed.put("manager", manager.get(null) != null);
                            android.content.ClipDescription description = ((ClipboardManager)activity.getSystemService(android.content.Context.CLIPBOARD_SERVICE)).getPrimaryClipDescription();
                            java.lang.reflect.Field since = ClipboardAccess.class.getDeclaredField("automaticSince");
                            java.lang.reflect.Field consumed = ClipboardAccess.class.getDeclaredField("consumedTimestamp");
                            since.setAccessible(true);
                            consumed.setAccessible(true);
                            observed.put("metadata", description != null);
                            if (description != null) {
                                long timestamp = description.getTimestamp();
                                observed.put("postConsent", timestamp > since.getLong(null));
                                observed.put("unconsumed", timestamp > consumed.getLong(null));
                                observed.put("notFuture", timestamp <= System.currentTimeMillis());
                            }
                            java.lang.reflect.Field cached = ClipboardAccess.class.getDeclaredField("cached");
                            java.lang.reflect.Field reported = ClipboardAccess.class.getDeclaredField("reportedChange");
                            cached.setAccessible(true);
                            reported.setAccessible(true);
                            observed.put("cached", cached.get(null) != null);
                            observed.put("synthetic", text.equals(cached.get(null)));
                            observed.put("changed", reported.getBoolean(null));
                            ClipData current = ((ClipboardManager)activity.getSystemService(android.content.Context.CLIPBOARD_SERVICE)).getPrimaryClip();
                            observed.put("osSynthetic", current != null && current.getItemCount() == 1
                                && text.contentEquals(current.getItemAt(0).getText() == null ? "" : current.getItemAt(0).getText()));
                            observed.put("remoteLabel", description != null && "Peersus".contentEquals(
                                description.getLabel() == null ? "" : description.getLabel()));
                            observed.put("sensitive", description != null && description.getExtras() != null
                                && description.getExtras().getBoolean("android.content.extra.IS_SENSITIVE", false));
                        } catch (Exception unavailable) { throw new AssertionError(unavailable); }
                    });
                    diagnostic = observed;
                } else if ("background".equals(action)) {
                    assertTrue(getInstrumentation().getUiAutomation().performGlobalAction(android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_HOME));
                    awaitFocus(activity, false);
                    backgrounded = true;
                } else if ("foreground".equals(action)) {
                    getInstrumentation().getTargetContext().startActivity(new Intent(Intent.ACTION_MAIN)
                        .setClass(activity, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP));
                    awaitFocus(activity, true);
                    foregrounded = true;
                } else fail("unknown synthetic validation action");
                sequence = next;
                status(status, sequence, true, diagnostic);
            }
            fail("synthetic clipboard validation timed out");
        } finally {
            Files.deleteIfExists(command.toPath());
            Files.deleteIfExists(new File(directory, "command.tmp").toPath());
            Files.deleteIfExists(status.toPath());
            Files.deleteIfExists(new File(directory, "status.tmp").toPath());
            assertTrue("validation files were not cleaned", directory.delete());
        }
    }

    private void awaitPairedTarget(MainActivity activity, String expectedPeer) throws Exception {
        long deadline = android.os.SystemClock.elapsedRealtime() + 30000;
        do {
            JSONObject snapshot = TestFrames.request(activity, new JSONObject().put("operation", "Snapshot")).getJSONObject("snapshot");
            org.json.JSONArray peers = snapshot.getJSONArray("peers");
            for (int i = 0; i < peers.length(); ++i) {
                JSONObject peer = peers.getJSONObject(i);
                if (expectedPeer.equals(peer.getString("id")) && peer.getBoolean("online")) return;
            }
            Thread.sleep(100);
        } while (android.os.SystemClock.elapsedRealtime() < deadline);
        fail("the exact physical target is not paired and online; no clipboard mutation was performed");
    }

    private void awaitFocus(MainActivity activity, boolean focused) throws Exception {
        long deadline = android.os.SystemClock.elapsedRealtime() + 10000;
        do {
            final boolean[] observed = {false};
            getInstrumentation().runOnMainSync(() -> observed[0] = activity.hasWindowFocus() == focused
                && (!focused || ((android.os.PowerManager)activity.getSystemService(android.content.Context.POWER_SERVICE)).isInteractive()));
            if (observed[0]) return;
            Thread.sleep(50);
        } while (android.os.SystemClock.elapsedRealtime() < deadline);
        fail("requested activity focus state unavailable");
    }

    private static void status(File destination, long sequence, boolean ok) throws Exception {
        status(destination, sequence, ok, null);
    }

    private static void status(File destination, long sequence, boolean ok, JSONObject diagnostic) throws Exception {
        File temporary = new File(destination.getParentFile(), "status.tmp");
        JSONObject result = new JSONObject().put("sequence", sequence).put("ok", ok);
        if (diagnostic != null) result.put("diagnostic", diagnostic);
        Files.write(temporary.toPath(), result.toString().getBytes(StandardCharsets.UTF_8));
        Files.move(temporary.toPath(), destination.toPath(), java.nio.file.StandardCopyOption.REPLACE_EXISTING);
    }
}
