package dev.peersus.app;

import java.io.IOException;
import java.util.ArrayList;
import java.util.List;

/** Plain-JVM regressions for the privileged operation, not Android setting-name assertions. */
public final class InputRecoverySettingsTest {
    private static final String OWN = "owner.app/owner.app.Input";
    private static final String OTHER = "other.app/other.app.Reader";
    private static final class Settings implements InputRecoverySettings.Backend {
        String services;
        boolean allowed = true, revokeDuringWait, changeDuringWait, failDuringWait;
        final List<String> writes = new ArrayList<>();
        Settings(String value) { services = value; }
        public boolean authorized() { return allowed; }
        public String read() { return services; }
        public void write(String value) { writes.add(value); services = value; }
        public void awaitDisconnect() throws IOException {
            if (revokeDuringWait) allowed = false;
            if (changeDuringWait) services = OTHER + ":new.app/new.app.Reader";
            if (failDuringWait) throw new IOException("interrupted recovery");
        }
    }
    private static void check(boolean value) { if (!value) throw new AssertionError(); }
    public static void main(String[] args) throws Exception {
        Settings absent = new Settings(OTHER);
        check(InputRecoverySettings.reconnect(absent, OWN) == InputRecoverySettings.Result.NOT_ALLOWED);
        check(absent.writes.isEmpty() && absent.services.equals(OTHER));
        Settings revoked = new Settings(OWN);
        revoked.allowed = false;
        check(InputRecoverySettings.reconnect(revoked, OWN) == InputRecoverySettings.Result.NOT_ALLOWED);
        check(revoked.writes.isEmpty());
        Settings existing = new Settings(OTHER + ":" + OWN);
        check(InputRecoverySettings.reconnect(existing, OWN) == InputRecoverySettings.Result.REQUESTED);
        check(existing.writes.equals(List.of(OTHER, OTHER + ":" + OWN)));
        Settings only = new Settings(OWN);
        check(InputRecoverySettings.reconnect(only, OWN) == InputRecoverySettings.Result.REQUESTED);
        check(only.writes.equals(List.of("", OWN)));
        Settings changed = new Settings(OTHER + ":" + OWN);
        changed.changeDuringWait = true;
        check(InputRecoverySettings.reconnect(changed, OWN) == InputRecoverySettings.Result.CHANGED);
        check(changed.writes.equals(List.of(OTHER)));
        check(changed.services.equals(OTHER + ":new.app/new.app.Reader"));
        Settings revokedInFlight = new Settings(OWN);
        revokedInFlight.revokeDuringWait = true;
        check(InputRecoverySettings.reconnect(revokedInFlight, OWN) == InputRecoverySettings.Result.NOT_ALLOWED);
        check(revokedInFlight.writes.equals(List.of("")));
        Settings interrupted = new Settings(OTHER + ":" + OWN);
        interrupted.failDuringWait = true;
        try { InputRecoverySettings.reconnect(interrupted, OWN); throw new AssertionError(); }
        catch (IOException expected) { check(interrupted.services.equals(OTHER + ":" + OWN)); }
        Settings duplicate = new Settings(OWN + ":" + OWN);
        check(InputRecoverySettings.reconnect(duplicate, OWN) == InputRecoverySettings.Result.NOT_ALLOWED);
        check(duplicate.writes.isEmpty());
        Settings off = new Settings(OTHER);
        check(InputRecoverySettings.enable(off, OWN) == InputRecoverySettings.Result.REQUESTED);
        check(off.writes.equals(List.of(OTHER + ":" + OWN)));
        Settings none = new Settings("");
        check(InputRecoverySettings.enable(none, OWN) == InputRecoverySettings.Result.REQUESTED);
        check(none.writes.equals(List.of(OWN)));
        Settings already = new Settings(OWN);
        check(InputRecoverySettings.enable(already, OWN) == InputRecoverySettings.Result.REQUESTED);
        check(already.writes.isEmpty());
        Settings refused = new Settings(OTHER);
        refused.allowed = false;
        check(InputRecoverySettings.enable(refused, OWN) == InputRecoverySettings.Result.NOT_ALLOWED);
        check(refused.writes.isEmpty());
        Settings on = new Settings(OTHER + ":" + OWN);
        check(InputRecoverySettings.disable(on, OWN) == InputRecoverySettings.Result.REQUESTED);
        check(on.writes.equals(List.of(OTHER)));
        Settings missing = new Settings(OTHER);
        check(InputRecoverySettings.disable(missing, OWN) == InputRecoverySettings.Result.REQUESTED);
        check(missing.writes.isEmpty());
        System.out.println("Accessibility recovery policy: 14 behavioral scenarios passed");
    }
}
