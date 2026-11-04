package dev.peersus.app;

import android.content.ClipData;
import android.content.ClipDescription;
import android.os.PersistableBundle;
import android.test.InstrumentationTestCase;

/** Uses real Android ClipData; never touches the device's actual clipboard. */
public final class ClipboardAccessTest extends InstrumentationTestCase {
    public void testReplacementIsValidatedUsingItsOwnDescription() {
        ClipData before = ClipData.newPlainText("before", "ordinary");
        ClipDescription stale = before.getDescription();
        assertTrue(stale.hasMimeType("text/plain"));
        ClipData replacement = sensitive("replacement");
        assertNull(ClipboardAccess.plainText(replacement));
        assertEquals("ordinary", ClipboardAccess.plainText(before));
    }

    public void testSensitiveReplacementWithIdenticalTextCannotBorrowConsent() {
        ClipData before = ClipData.newPlainText("before", "same text");
        assertEquals("same text", ClipboardAccess.plainText(before));
        assertNull(ClipboardAccess.plainText(sensitive("same text")));
    }

    public void testUriIsNotCoercedToText() {
        ClipData clip = ClipData.newRawUri("uri", android.net.Uri.parse("content://example/item"));
        assertNull(ClipboardAccess.plainText(clip));
    }

    public void testPlainTextMimeCannotSmuggleUriOrIntent() {
        ClipData uri = new ClipData(new ClipDescription("mixed", new String[]{"text/plain"}),
            new ClipData.Item("ordinary", null, null, android.net.Uri.parse("content://example/item")));
        assertNull(ClipboardAccess.plainText(uri));
        ClipData intent = new ClipData(new ClipDescription("mixed", new String[]{"text/plain"}),
            new ClipData.Item("ordinary", null, new android.content.Intent("example.action"), null));
        assertNull(ClipboardAccess.plainText(intent));
    }

    public void testMultipleAndOversizedClipsAreRejected() {
        ClipData multiple = ClipData.newPlainText("multiple", "ordinary");
        multiple.addItem(new ClipData.Item("another"));
        assertNull(ClipboardAccess.plainText(multiple));
        assertNull(ClipboardAccess.plainText(ClipData.newPlainText("large", "a".repeat(ClipboardAccess.MAX_BYTES + 1))));
        assertNull(ClipboardAccess.plainText(ClipData.newPlainText("utf8", "€".repeat(ClipboardAccess.MAX_BYTES / 3 + 1))));
    }

    public void testSelectedPayloadIsExactOneShotAndExpiresWithoutFallback() {
        ClipboardConsent consent = new ClipboardConsent();
        consent.select(1, "selected", 1000);
        assertNull(consent.take(2, 500));
        assertEquals("selected", consent.take(1, 500));
        assertNull(consent.take(1, 501));
        assertTrue(consent.valid(1, 501));
        consent.clear();
        assertFalse(consent.valid(1, 502));
        consent.select(3, "expired", 1000);
        assertNull(consent.take(3, 1000));
        consent.select(4, "replacement", 2000);
        assertNull(consent.take(3, 1100));
        assertEquals("replacement", consent.take(4, 1100));
    }

    public void testCancelledOrExpiredQueuedWriteHasNoPermit() {
        ClipboardRequest queued = new ClipboardRequest(1000);
        assertTrue(queued.allowed(500));
        queued.cancel();
        assertFalse(queued.allowed(501));
        ClipboardRequest expired = new ClipboardRequest(1000);
        assertFalse(expired.allowed(1000));
        assertFalse(expired.allowed(3500));
    }

    public void testBackgroundCopyNewerThanOptInRemainsFreshOnResume() {
        assertTrue(ClipboardAccess.freshCopy(2000, 9000, 1000, 1500));
        assertFalse(ClipboardAccess.freshCopy(2000, 9000, 1000, 2000));
        assertTrue(ClipboardAccess.freshCopy(3000, 9000, 1000, 2000));
    }

    public void testOldFutureAndRevokedEpochCopiesAreNotReplayed() {
        assertFalse(ClipboardAccess.freshCopy(999, 9000, 1000, 0));
        assertFalse(ClipboardAccess.freshCopy(1000, 9000, 1000, 0));
        assertFalse(ClipboardAccess.freshCopy(9001, 9000, 1000, 1500));
        assertFalse(ClipboardAccess.freshCopy(2000, 9000, 3000, 1500));
        assertTrue(ClipboardAccess.freshCopy(4000, 9000, 3000, 2000));
    }

    public void testAutomaticAcceptanceRejectsOldSensitiveDuplicateAndRevokedCopies() throws Exception {
        getInstrumentation().runOnMainSync(ClipboardAccess::stopAutomatic);
        Thread.sleep(5);
        long first = System.currentTimeMillis();
        getInstrumentation().runOnMainSync(() -> {
            assertNull(ClipboardAccess.acceptClip(dated("stale", 1, false), false));
            assertFalse(ClipboardAccess.takeChanged());
            assertNull(ClipboardAccess.acceptClip(dated("synthetic-sensitive", first, true), false));
            assertFalse(ClipboardAccess.takeChanged());
        });
        Thread.sleep(5);
        long next = System.currentTimeMillis();
        getInstrumentation().runOnMainSync(() -> {
            ClipData copy = dated("synthetic-ordinary", next, false);
            assertEquals("synthetic-ordinary", ClipboardAccess.acceptClip(copy, false));
            assertTrue(ClipboardAccess.takeChanged());
            assertFalse(ClipboardAccess.takeChanged());
            assertNull(ClipboardAccess.acceptClip(copy, false));
            ClipboardAccess.stopAutomatic();
            assertNull(ClipboardAccess.acceptClip(copy, false));
            assertFalse(ClipboardAccess.takeChanged());
        });
    }

    private static ClipData dated(String text, long timestamp, boolean sensitive) {
        ClipDescription description = new ClipDescription("synthetic", new String[]{"text/plain"}) {
            @Override public long getTimestamp() { return timestamp; }
        };
        if (sensitive) {
            PersistableBundle extras = new PersistableBundle();
            extras.putBoolean("android.content.extra.IS_SENSITIVE", true);
            description.setExtras(extras);
        }
        return new ClipData(description, new ClipData.Item(text));
    }

    private static ClipData sensitive(String text) {
        ClipData clip = ClipData.newPlainText("sensitive", text);
        PersistableBundle extras = new PersistableBundle();
        extras.putBoolean("android.content.extra.IS_SENSITIVE", true);
        clip.getDescription().setExtras(extras);
        return clip;
    }
}
