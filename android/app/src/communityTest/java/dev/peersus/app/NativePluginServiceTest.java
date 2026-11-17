package dev.peersus.app;

import android.content.Context;
import android.net.*;
import android.os.*;
import static org.junit.Assert.*;
import org.junit.Test;
import static androidx.test.platform.app.InstrumentationRegistry.getInstrumentation;
import java.io.*;
import java.nio.ByteBuffer;
import org.json.JSONObject;
import static dev.peersus.app.TestFrames.*;

/** Real AMS slots and real SDK modules; no identity, clipboard or paired-device access. */
public final class NativePluginServiceTest {
    @Test
    public void testTwoNativeModulesKeepDistinctProcessesAndStopIndependently() throws Exception {
        try (Worker first = new Worker("echo.so"); Worker second = new Worker("echo.so")) {
            assertTrue(first.pid != second.pid);
            first.echo(7); second.echo(8);
            first.stop();
            second.echo(9);
        }
    }
    @Test
    public void testNativeBulkDataUsesTheSameTicketAndHalfCloseContract() throws Exception {
        try (Worker worker = new Worker("data.so")) {
            worker.call(7, "data");
            readable(worker.dataServer.getFileDescriptor());
            try (LocalSocket data = worker.dataServer.accept()) {
                nonblocking(data.getFileDescriptor());
                assertEquals(worker.pid, data.getPeerCredentials().getPid());
                assertEquals(android.os.Process.myUid(), data.getPeerCredentials().getUid());
                JSONObject hello = readFrame(data.getFileDescriptor());
                assertEquals(worker.token, hello.getString("token"));
                assertEquals(7L, hello.getLong("ticket"));
                byte[] bytes = new byte[9]; transfer(data.getFileDescriptor(), bytes, false);
                assertEquals("real data", new String(bytes, java.nio.charset.StandardCharsets.UTF_8));
                readable(data.getFileDescriptor());
                assertEquals(0, android.system.Os.read(data.getFileDescriptor(), new byte[1], 0, 1));
            }
            JSONObject reply = readFrame(worker.control.getFileDescriptor());
            assertEquals(7L, reply.getLong("ticket"));
            assertEquals("real descriptor data", reply.getJSONObject("result").getString("value"));
        }
    }
    private final class Worker implements AutoCloseable {
        final Context context = getInstrumentation().getTargetContext();
        final long generation = (1L << 59) + android.os.SystemClock.elapsedRealtimeNanos();
        final String token = "c".repeat(64);
        final File directory, module, controlPath, dataPath;
        final LocalSocket controlBinding = new LocalSocket(), dataBinding = new LocalSocket();
        final LocalServerSocket controlServer, dataServer;
        LocalSocket control;
        int pid;
        boolean launched;
        Worker(String fixture) throws Exception {
            System.loadLibrary("peersus_android");
            directory = java.nio.file.Files.createTempDirectory(context.getCacheDir().toPath(), "native-test-").toFile();
            module = new File(directory, fixture); controlPath = new File(directory, "c.sock"); dataPath = new File(directory, "d.sock");
            controlBinding.bind(new LocalSocketAddress(controlPath.getAbsolutePath(), LocalSocketAddress.Namespace.FILESYSTEM));
            dataBinding.bind(new LocalSocketAddress(dataPath.getAbsolutePath(), LocalSocketAddress.Namespace.FILESYSTEM));
            controlServer = new LocalServerSocket(controlBinding.getFileDescriptor());
            dataServer = new LocalServerSocket(dataBinding.getFileDescriptor());
            nonblocking(controlServer.getFileDescriptor()); nonblocking(dataServer.getFileDescriptor());
            try {
                try (InputStream input = getInstrumentation().getContext().getAssets().open(android.os.Build.SUPPORTED_ABIS[0] + "/" + fixture);
                     FileOutputStream output = new FileOutputStream(module)) { input.transferTo(output); }
                assertTrue(module.setReadOnly());
                assertEquals(0, android.system.Os.stat(module.getAbsolutePath()).st_mode & 0222);
                assertTrue(NativeModules.launch(context, generation, module.getAbsolutePath(), controlPath.getAbsolutePath(), dataPath.getAbsolutePath(), token));
                launched = true;
                java.util.concurrent.atomic.AtomicReference<Object> observedSession = new java.util.concurrent.atomic.AtomicReference<>();
                getInstrumentation().runOnMainSync(() -> {
                    try {
                        java.lang.reflect.Field field = NativeModules.class.getDeclaredField("sessions"); field.setAccessible(true);
                        observedSession.set(((java.util.Map<?, ?>)field.get(null)).get(generation));
                    } catch (ReflectiveOperationException error) { throw new AssertionError(error); }
                });
                try { readable(controlServer.getFileDescriptor()); }
                catch (IOException unavailable) {
                    java.util.concurrent.atomic.AtomicReference<String> state = new java.util.concurrent.atomic.AtomicReference<>("unobserved");
                    getInstrumentation().runOnMainSync(() -> {
                        try {
                            Object session = observedSession.get();
                            if (session == null) state.set("session-ended-before-connect");
                            else {
                                StringBuilder result = new StringBuilder();
                                for (String key : new String[]{"slot", "bound", "stopping", "finished", "pid", "exitCode"}) {
                                    java.lang.reflect.Field detail = session.getClass().getDeclaredField(key); detail.setAccessible(true);
                                    result.append(key).append('=').append(key.equals("pid") ? detail.getInt(session) > 0 : detail.get(session)).append(' ');
                                }
                                state.set(result.toString());
                            }
                        } catch (ReflectiveOperationException error) { state.set(error.getClass().getSimpleName()); }
                    });
                    throw new IOException("native fixture " + fixture + " did not connect: " + state.get(), unavailable);
                }
                control = controlServer.accept(); nonblocking(control.getFileDescriptor());
                pid = control.getPeerCredentials().getPid();
                assertTrue(pid > 0 && pid != android.os.Process.myPid());
                assertEquals(android.os.Process.myUid(), control.getPeerCredentials().getUid());
                assertEquals(token, readFrame(control.getFileDescriptor()).getString("token"));
            } catch (Exception | AssertionError failure) { close(); throw failure; }
        }
        void call(long ticket, String method) throws Exception {
            byte[] call = ("{\"type\":\"call\",\"ticket\":" + ticket + ",\"caller\":\"host\",\"service\":\"echo\",\"version\":1,\"method\":\"" + method + "\",\"args\":null}").getBytes(java.nio.charset.StandardCharsets.UTF_8);
            transfer(control.getFileDescriptor(), ByteBuffer.allocate(4 + call.length).putInt(call.length).put(call).array(), true);
        }
        void echo(long ticket) throws Exception {
            call(ticket, "echo"); JSONObject reply = readFrame(control.getFileDescriptor());
            assertEquals(ticket, reply.getLong("ticket")); assertEquals("portable echo", reply.getJSONObject("result").getString("value"));
        }
        void stop() throws Exception {
            if (!launched) return;
            NativeModules.stop(generation);
            long deadline = android.os.SystemClock.elapsedRealtime() + 5000;
            while (pid > 0 && alive(pid) && android.os.SystemClock.elapsedRealtime() < deadline) Thread.sleep(10);
            assertFalse("native worker survived generation stop", pid > 0 && alive(pid));
            launched = false;
        }
        boolean alive(int target) {
            java.util.List<android.app.ActivityManager.RunningAppProcessInfo> running = context.getSystemService(android.app.ActivityManager.class).getRunningAppProcesses();
            if (running == null) throw new AssertionError("Process lifecycle evidence is unavailable");
            for (android.app.ActivityManager.RunningAppProcessInfo process : running) if (process.pid == target && process.uid == android.os.Process.myUid()) return true;
            return false;
        }
        @Override public void close() throws Exception {
            try { stop(); } finally {
                if (control != null) control.close(); controlBinding.close(); dataBinding.close();
                if (module.exists()) assertTrue(module.delete());
                if (controlPath.exists()) assertTrue(controlPath.delete());
                if (dataPath.exists()) assertTrue(dataPath.delete());
                assertTrue(directory.delete());
            }
        }
    }
}
