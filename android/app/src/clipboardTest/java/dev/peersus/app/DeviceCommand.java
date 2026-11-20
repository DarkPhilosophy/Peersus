package dev.peersus.app;

import static androidx.test.platform.app.InstrumentationRegistry.getInstrumentation;
import static org.junit.Assert.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;

/** What a clipboard test asks of the device itself: one command of the test's own, run as the device's test user. */
final class DeviceCommand {
    /** More than a test's command may print. */
    private static final int MOST_OUTPUT = 32768;

    private DeviceCommand() {}

    /** Runs `command` and returns what it printed. */
    static String output(String command) throws Exception {
        try (InputStream input = new android.os.ParcelFileDescriptor.AutoCloseInputStream(
                getInstrumentation().getUiAutomation().executeShellCommand(command));
                ByteArrayOutputStream output = new ByteArrayOutputStream()) {
            byte[] buffer = new byte[1024];
            int count;
            while ((count = input.read(buffer)) != -1) {
                assertTrue("test command output exceeded limit", output.size() + count <= MOST_OUTPUT);
                output.write(buffer, 0, count);
            }
            return new String(output.toByteArray(), StandardCharsets.UTF_8);
        }
    }
}
