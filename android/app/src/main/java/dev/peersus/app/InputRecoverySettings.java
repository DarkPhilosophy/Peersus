package dev.peersus.app;

import java.io.IOException;
import java.util.ArrayList;
import java.util.List;

/**
 * The only changes made to the accessibility list, always to this app's own service and never another's: turn it on or off when the
 * person flips its switch, and rebind it when it is on but disconnected. The global accessibility flag is never turned on here when it
 * would also turn on services the person had left off.
 */
final class InputRecoverySettings {
    enum Result { REQUESTED, NOT_ALLOWED, CHANGED }
    interface Backend {
        boolean authorized() throws IOException;
        String read() throws IOException;
        void write(String services) throws IOException;
        void awaitDisconnect() throws IOException;
    }
    /** Turn this app's service on: add it once, keeping every other entry and refusing a concurrent change. */
    static Result enable(Backend backend, String own) throws IOException {
        if (!backend.authorized()) return Result.NOT_ALLOWED;
        String before = backend.read();
        if (before == null || before.length() > 65536) return Result.NOT_ALLOWED;
        for (String value : before.split(":", -1)) if (value.equals(own)) return Result.REQUESTED;
        if (!backend.authorized()) return Result.NOT_ALLOWED;
        if (!before.equals(backend.read())) return Result.CHANGED;
        backend.write(before.isEmpty() ? own : before + ":" + own);
        return Result.REQUESTED;
    }
    /** Turn this app's service off: remove only its own entries, keeping every other one. */
    static Result disable(Backend backend, String own) throws IOException {
        if (!backend.authorized()) return Result.NOT_ALLOWED;
        String before = backend.read();
        if (before == null || before.length() > 65536) return Result.NOT_ALLOWED;
        List<String> remaining = new ArrayList<>();
        boolean present = false;
        for (String value : before.split(":", -1)) {
            if (value.equals(own)) present = true;
            else if (!value.isEmpty()) remaining.add(value);
        }
        if (!present) return Result.REQUESTED;
        if (!backend.authorized()) return Result.NOT_ALLOWED;
        if (!before.equals(backend.read())) return Result.CHANGED;
        backend.write(String.join(":", remaining));
        return Result.REQUESTED;
    }
    static Result reconnect(Backend backend, String own) throws IOException {
        if (!backend.authorized()) return Result.NOT_ALLOWED;
        String before = backend.read();
        if (before == null || before.length() > 65536) return Result.NOT_ALLOWED;
        List<String> remaining = new ArrayList<>();
        int matches = 0;
        for (String value : before.split(":", -1)) {
            if (value.equals(own)) matches++;
            else if (!value.isEmpty()) remaining.add(value);
        }
        if (matches != 1) return Result.NOT_ALLOWED;
        String without = String.join(":", remaining);
        if (!backend.authorized()) return Result.NOT_ALLOWED;
        if (!before.equals(backend.read())) return Result.CHANGED;
        try {
            backend.write(without);
            backend.awaitDisconnect();
            if (!backend.authorized()) return Result.NOT_ALLOWED;
            // A concurrent settings change belongs to the person, not this recovery.
            if (!without.equals(backend.read())) return Result.CHANGED;
            backend.write(before);
            return Result.REQUESTED;
        } catch (IOException error) {
            // Roll back only our own removal, only while consent and the exact interim state remain.
            try {
                if (backend.authorized() && without.equals(backend.read())) backend.write(before);
            } catch (IOException rollback) { error.addSuppressed(rollback); }
            throw error;
        }
    }
    private InputRecoverySettings() { }
}
