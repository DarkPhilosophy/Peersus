package dev.peersus.app;

import android.content.ComponentName;
import android.content.Context;
import android.os.Binder;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.TimeUnit;

/**
 * Optional shell UserService that hands Android one update of this application. The owner has already verified the bytes against the
 * signed release; here Android is told the package they must be (`--pkg`), so that nothing else can be installed through this service,
 * and Android itself refuses a package signed by anyone else or older than the one installed.
 *
 * Replacing the package ends the owner, and with it this worker. Opening the application again is therefore left to the shell command,
 * which outlives both.
 */
public final class AppUpdateUserService extends IAppUpdate.Stub {
    static final long MAX_BYTES = 128L * 1024 * 1024;
    static final int MAX_CHUNK = 256 * 1024;
    /** Results: 0 done, 3 not usable as asked, 4 Android refused it, 5 it took too long. */
    private static final int MISUSED = 3, REFUSED = 4, LATE = 5;
    // `$1` user, `$2` package, `$3` size, `$4` the window to open afterwards or nothing. The pipe to this worker is gone by the time
    // the package is replaced, so that signal is ignored and what Android said is kept until the end.
    private static final String SCRIPT =
        "trap '' PIPE; said=$(/system/bin/pm install -r --user \"$1\" --pkg \"$2\" -S \"$3\" 2>&1); code=$?; "
        + "if [ \"$code\" -eq 0 ] && [ -n \"$4\" ]; then /system/bin/am start --user \"$1\" -n \"$4\" </dev/null >/dev/null 2>&1; fi; "
        + "echo \"$said\"; exit \"$code\"";
    private final int ownerUid;
    private final String user, packageName, window;
    private final java.util.concurrent.ScheduledThreadPoolExecutor deadlines =
        new java.util.concurrent.ScheduledThreadPoolExecutor(1, task -> {
            Thread thread = new Thread(task, "app-update-deadline");
            thread.setDaemon(true);
            return thread;
        });
    private Process process;
    private OutputStream input;
    private long remaining;
    private java.util.concurrent.ScheduledFuture<?> deadline;

    public AppUpdateUserService(Context context) {
        ownerUid = context.getApplicationInfo().uid;
        user = Integer.toString(ownerUid / 100000);
        packageName = context.getPackageName();
        window = new ComponentName(context, MainActivity.class).flattenToString();
    }

    private void owner() {
        if (Binder.getCallingUid() != ownerUid || android.os.Process.myUid() != 2000)
            throw new SecurityException("Updating requires the application itself and a shell provider");
    }

    @Override public synchronized int begin(long size, boolean reopen) {
        owner();
        if (process != null || size < 1 || size > MAX_BYTES) return MISUSED;
        try {
            process = new ProcessBuilder("/system/bin/sh", "-c", SCRIPT, "update", user, packageName, Long.toString(size),
                reopen ? window : "").redirectErrorStream(true).start();
        } catch (IOException unavailable) { return REFUSED; }
        input = process.getOutputStream();
        remaining = size;
        deadlines.setRemoveOnCancelPolicy(true);
        deadline = deadlines.schedule(process::destroyForcibly, 5, TimeUnit.MINUTES);
        return 0;
    }

    @Override public synchronized int write(byte[] chunk) {
        owner();
        if (input == null || chunk == null || chunk.length == 0 || chunk.length > MAX_CHUNK || chunk.length > remaining) {
            abandon();
            return MISUSED;
        }
        try {
            input.write(chunk);
            remaining -= chunk.length;
            return 0;
        } catch (IOException refused) {
            // Android stopped reading: what it said is read at the end.
            return finishLocked();
        }
    }

    /** Returns to a living caller only when Android did not replace the application. */
    @Override public synchronized int finish() {
        owner();
        if (input == null || remaining != 0) {
            abandon();
            return MISUSED;
        }
        return finishLocked();
    }

    private int finishLocked() {
        Process running = process;
        if (running == null) return MISUSED;
        try {
            try { input.close(); } catch (IOException closed) { /* Android already ended the command. */ }
            byte[] said = running.getInputStream().readNBytes(4096);
            if (!running.waitFor(4, TimeUnit.MINUTES)) return LATE;
            if (running.exitValue() == 0) return 0;
            // Android's own refusal (a different signer, an older build): no content of the application is in it.
            android.util.Log.w("Peersus", "Android refused the update: "
                + new String(said, StandardCharsets.UTF_8).replaceAll("[^\\x20-\\x7e]", " ").trim());
            return REFUSED;
        } catch (IOException unreadable) {
            return REFUSED;
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return LATE;
        } finally {
            abandon();
        }
    }

    private void abandon() {
        if (deadline != null) deadline.cancel(false);
        deadline = null;
        Process running = process;
        process = null;
        input = null;
        remaining = 0;
        if (running == null) return;
        if (running.isAlive()) running.destroyForcibly();
        try { running.getOutputStream().close(); } catch (IOException ignored) { }
        try { running.getInputStream().close(); } catch (IOException ignored) { }
        try { running.getErrorStream().close(); } catch (IOException ignored) { }
    }

    @Override public void destroy() {
        if (Binder.getCallingUid() != ownerUid && Binder.getCallingUid() != android.os.Process.myUid())
            throw new SecurityException();
        System.exit(0);
    }
}
