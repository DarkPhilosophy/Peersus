package dev.peersus.app;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.test.ActivityInstrumentationTestCase2;

/** Opt-in only: a fresh process receives fixed synthetic text before any Activity is attached. */
public final class ClipboardBackgroundReceiveTest extends ActivityInstrumentationTestCase2<MainActivity> {
    public ClipboardBackgroundReceiveTest() { super(MainActivity.class); }

    public void testApplicationContextReceivesBeforeAnyActivityExists() throws Exception {
        android.os.Bundle args = ((android.test.InstrumentationTestRunner)getInstrumentation()).getArguments();
        assertEquals("synthetic clipboard writes require explicit opt-in", "true", args.getString("synthetic_clipboard"));
        System.loadLibrary("peersus_android");
        String text = SyntheticCopyActivity.text(16);
        getInstrumentation().runOnMainSync(() -> {
            PluginBridge.initialize(getInstrumentation().getTargetContext());
            assertNull("a service-only context must not read the clipboard", ClipboardAccess.read(true));
            assertTrue("authorized background receive requires no Activity", ClipboardAccess.write(text, () -> true));
            assertNull("receiving must not grant background read access", ClipboardAccess.read(false));
        });
        setActivityIntent(new Intent(Intent.ACTION_MAIN).setClass(getInstrumentation().getTargetContext(), MainActivity.class)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
        MainActivity activity = getActivity();
        long deadline = android.os.SystemClock.elapsedRealtime() + 10000;
        boolean[] focused = {false};
        do {
            getInstrumentation().runOnMainSync(() -> focused[0] = activity.hasWindowFocus());
            if (focused[0]) break;
            Thread.sleep(25);
        } while (android.os.SystemClock.elapsedRealtime() < deadline);
        assertTrue("foreground verification requires real focus", focused[0]);
        getInstrumentation().runOnMainSync(() -> {
            ClipData clip = ((ClipboardManager)activity.getSystemService(android.content.Context.CLIPBOARD_SERVICE)).getPrimaryClip();
            assertNotNull("synthetic background write was not retained", clip);
            assertEquals(1, clip.getItemCount());
            assertEquals("Peersus", clip.getDescription().getLabel());
            assertEquals(text, clip.getItemAt(0).getText());
        });
    }
}
