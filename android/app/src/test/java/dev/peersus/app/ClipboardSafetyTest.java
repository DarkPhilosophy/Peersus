package dev.peersus.app;

/** Pure JVM regressions: run with javac/java; no Android SDK stand-ins or clipboard access. */
public final class ClipboardSafetyTest {
    private static void check(boolean value) { if (!value) throw new AssertionError(); }

    public static void main(String[] args) {
        ClipboardConsent consent = new ClipboardConsent();
        consent.select(41, "selected", 3000);
        check(consent.take(40, 1000) == null); // Old action cannot consume a newer selection.
        check("selected".equals(consent.take(41, 1000)));
        check(consent.take(41, 1001) == null); // One shot, including identical payloads.
        check(consent.valid(41, 1001)); // A captured payload can still commit before revocation.
        consent.clear();
        check(!consent.valid(41, 1002)); // Pause/replacement revokes a pending send too.
        check(consent.take(41, 1002) == null);
        consent.select(42, "new selected", 3000);
        check(consent.take(41, 2000) == null);
        check("new selected".equals(consent.take(42, 2000)));
        consent.select(43, "expired", 3000);
        check(consent.take(43, 3000) == null);
        check(!consent.valid(43, 3000));
        consent.select(44, "cancelled", 4000);
        consent.clear();
        check(consent.take(44, 3001) == null);

        ClipboardRequest queued = new ClipboardRequest(12000);
        check(queued.allowed(11000));
        queued.cancel(); // Future dropped while the main-handler WRITE is still queued.
        check(!queued.allowed(11001));
        ClipboardRequest expired = new ClipboardRequest(12000);
        check(!expired.allowed(12000));
        check(!expired.allowed(14500)); // Executing a queue turn never renews its budget.
    }
}
