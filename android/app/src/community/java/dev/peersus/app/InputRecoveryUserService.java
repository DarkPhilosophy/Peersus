package dev.peersus.app;

import android.content.ComponentName;
import android.content.Context;
import android.os.Binder;
import android.os.RemoteException;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.TimeUnit;

/** Optional shell UserService. It cannot grant a missing service or target another application's service. */
public final class InputRecoveryUserService extends IInputRecovery.Stub {
    private final int ownerUid;
    private final String component, user;
    private final java.util.concurrent.ScheduledThreadPoolExecutor deadlines =
        new java.util.concurrent.ScheduledThreadPoolExecutor(1, task -> {
            Thread thread = new Thread(task, "input-recovery-deadline");
            thread.setDaemon(true);
            return thread;
        });
    public InputRecoveryUserService(Context context) {
        ownerUid = context.getApplicationInfo().uid;
        user = Integer.toString(ownerUid / 100000);
        component = new ComponentName(context, RemoteInput.class).flattenToString();
    }
    @Override public int reconnect(IInputRecoveryConsent consent) { return run(consent, 0); }
    @Override public int enable(IInputRecoveryConsent consent) { return run(consent, 1); }
    @Override public int disable(IInputRecoveryConsent consent) { return run(consent, 2); }
    private synchronized int run(IInputRecoveryConsent consent, int operation) {
        if (Binder.getCallingUid() != ownerUid || android.os.Process.myUid() != 2000)
            throw new SecurityException("Input recovery requires its owner and a shell provider");
        if (consent == null) return 1;
        java.util.concurrent.atomic.AtomicBoolean alive = new java.util.concurrent.atomic.AtomicBoolean(true);
        android.os.IBinder.DeathRecipient died = () -> alive.set(false);
        try {
            consent.asBinder().linkToDeath(died, 0);
            InputRecoverySettings.Backend backend = new InputRecoverySettings.Backend() {
                public boolean authorized() {
                    try { return alive.get() && consent.allowed(); }
                    catch (RemoteException unavailable) { return false; }
                }
                public String read() throws IOException {
                    String value = setting("enabled_accessibility_services", null).strip();
                    return value.equals("null") ? "" : value;
                }
                public void write(String value) throws IOException { setting("enabled_accessibility_services", value); }
                public void awaitDisconnect() throws IOException {
                    long until = android.os.SystemClock.elapsedRealtime() + 3000;
                    do {
                        String state = command(new ProcessBuilder("/system/bin/dumpsys", "accessibility"));
                        boolean removed = true;
                        for (String label : new String[]{"Enabled services:{", "Binding services:{", "Crashed services:{"}) {
                            int start = state.indexOf(label);
                            if (start < 0) throw new IOException("Accessibility state unavailable");
                            int end = state.indexOf('\n', start);
                            if (end < 0) end = state.length();
                            if (state.substring(start, end).contains(component)) removed = false;
                        }
                        if (removed) return;
                        try { Thread.sleep(100); }
                        catch (InterruptedException interrupted) {
                            Thread.currentThread().interrupt();
                            throw new IOException("Recovery interrupted");
                        }
                    } while (alive.get() && android.os.SystemClock.elapsedRealtime() < until);
                    throw new IOException("Android did not unbind the input service");
                }
            };
            if (operation == 2) return InputRecoverySettings.disable(backend, component).ordinal();
            if (operation == 0) return InputRecoverySettings.reconnect(backend, component).ordinal();
            int result = InputRecoverySettings.enable(backend, component).ordinal();
            if (result != 0) return result;
            // The global switch also turns on every other listed service: turn it on only when ours is the only one listed.
            String global = setting("accessibility_enabled", null).strip();
            if (!global.equals("1")) {
                if (!backend.read().equals(component)) {
                    InputRecoverySettings.disable(backend, component);
                    return 11;
                }
                setting("accessibility_enabled", "1");
            }
            return 0;
        } catch (IOException | RuntimeException | RemoteException unavailable) {
            // Device logging may be unavailable; a class-only code is returned instead of text.
            if (unavailable instanceof Step) return ((Step) unavailable).code;
            return unavailable instanceof IOException ? 3 : unavailable instanceof SecurityException ? 4 : 5;
        }
        finally {
            try { consent.asBinder().unlinkToDeath(died, 0); }
            catch (java.util.NoSuchElementException ignored) { }
        }
    }
    /** A bounded, text-free failure stage of one settings command. */
    private static final class Step extends IOException {
        final int code;
        Step(int code) { this.code = code; }
    }
    /** Read (`value` null) or write one of the two fixed accessibility keys; no caller supplies a key. */
    private String setting(String key, String value) throws IOException {
        ProcessBuilder command = new ProcessBuilder("/system/bin/settings", "--user", user,
            value == null ? "get" : "put", "secure", key);
        if (value != null) command.command().add(value);
        return command(command);
    }
    private String command(ProcessBuilder command) throws IOException {
        final Process process;
        try { process = command.redirectErrorStream(true).start(); }
        catch (IOException unavailable) { throw new Step(9); }
        deadlines.setRemoveOnCancelPolicy(true);
        java.util.concurrent.ScheduledFuture<?> deadline =
            deadlines.schedule(process::destroyForcibly, 2, TimeUnit.SECONDS);
        try {
            byte[] output = process.getInputStream().readNBytes(65538);
            if (!process.waitFor(1500, TimeUnit.MILLISECONDS)) throw new Step(10);
            if (process.exitValue() != 0) throw new Step(20 + Math.min(Math.max(process.exitValue(), 0), 9));
            if (output.length > 65536) throw new IOException("Accessibility configuration too large");
            return new String(output, StandardCharsets.UTF_8);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            throw new IOException("Recovery interrupted");
        } finally {
            deadline.cancel(false);
            if (process.isAlive()) process.destroyForcibly();
            process.getInputStream().close();
            process.getErrorStream().close();
            process.getOutputStream().close();
        }
    }
    @Override public void destroy() {
        if (Binder.getCallingUid() != ownerUid && Binder.getCallingUid() != android.os.Process.myUid())
            throw new SecurityException();
        System.exit(0);
    }
}
