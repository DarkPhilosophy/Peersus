package dev.peersus.app;

import android.content.Context;
import android.os.Binder;
import java.io.BufferedReader;
import java.io.FileReader;
import java.io.IOException;

/**
 * Optional shell UserService that reads what Android closes to an application and opens to the shell: the processors' ticks of
 * /proc/stat. It reads one fixed file, answers only this application, and writes nothing.
 */
public final class ReadingsUserService extends IReadings.Stub {
    private final int ownerUid;
    public ReadingsUserService(Context context) { ownerUid = context.getApplicationInfo().uid; }
    @Override public String processorTime() {
        if (Binder.getCallingUid() != ownerUid) throw new SecurityException("Readings answer their owner only");
        try (BufferedReader stat = new BufferedReader(new FileReader("/proc/stat"), 512)) {
            String line = stat.readLine();
            return line != null && line.startsWith("cpu ") && line.length() <= 256 ? line : null;
        } catch (IOException unavailable) { return null; }
    }
    @Override public void destroy() {
        if (Binder.getCallingUid() != ownerUid && Binder.getCallingUid() != android.os.Process.myUid())
            throw new SecurityException();
        System.exit(0);
    }
}
