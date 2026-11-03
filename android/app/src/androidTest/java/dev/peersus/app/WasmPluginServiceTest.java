package dev.peersus.app;

import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.IBinder;
import android.test.InstrumentationTestCase;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;
import static dev.peersus.app.TestFrames.*;

/** Actual AMS binding, not a mocked Service or launcher. No device identity or clipboard is read. */
public final class WasmPluginServiceTest extends InstrumentationTestCase {
    public void testNamedIsolatedServiceAcceptsItsBootstrapWithoutRelyingOnAProcessLabel() throws Exception {
        Context context = getInstrumentation().getTargetContext();
        CountDownLatch arrived = new CountDownLatch(1);
        AtomicReference<IBinder> binder = new AtomicReference<>();
        AtomicReference<String> failure = new AtomicReference<>();
        AtomicBoolean bound = new AtomicBoolean();
        ServiceConnection connection = new ServiceConnection() {
            @Override public void onServiceConnected(ComponentName name, IBinder service) { binder.set(service); arrived.countDown(); }
            @Override public void onNullBinding(ComponentName name) { failure.set("isolated service rejected its binding"); arrived.countDown(); }
            @Override public void onBindingDied(ComponentName name) { failure.set("isolated binding died"); arrived.countDown(); }
            @Override public void onServiceDisconnected(ComponentName name) { }
        };
        try {
            getInstrumentation().runOnMainSync(() -> {
                Intent intent = new Intent(context, WasmPluginService.class);
                intent.putExtra("generation", 123L);
                try {
                    bound.set(context.bindIsolatedService(intent, Context.BIND_AUTO_CREATE | Context.BIND_IMPORTANT,
                        "binding_test_" + android.os.SystemClock.elapsedRealtime(), context.getMainExecutor(), connection));
                    if (!bound.get()) { failure.set("framework refused binding"); arrived.countDown(); }
                } catch (RuntimeException unavailable) { failure.set(unavailable.getClass().getSimpleName()); arrived.countDown(); }
            });
            assertTrue("isolated binding callback timed out", arrived.await(10, TimeUnit.SECONDS));
            assertNull(failure.get(), failure.get());
            assertNotNull("isolated service must expose the private bootstrap Binder", binder.get());
            assertTrue(binder.get().isBinderAlive());
        } finally {
            if (bound.get()) getInstrumentation().runOnMainSync(() -> context.unbindService(connection));
        }
    }

    public void testDescriptorBootstrapRunsTheActualInterpreterAndAnswersTheSdkAction() throws Exception {
        Context context = getInstrumentation().getTargetContext();
        mark("copying-sdk-asset");
        java.io.File module = java.io.File.createTempFile("wasm-binding-", ".wasm", context.getCacheDir());
        try (java.io.InputStream input = getInstrumentation().getContext().getAssets().open("echo.wasm");
             java.io.FileOutputStream output = new java.io.FileOutputStream(module)) { input.transferTo(output); }
        mark("sdk-asset-ready");
        android.os.ParcelFileDescriptor[] control = android.os.ParcelFileDescriptor.createSocketPair();
        android.system.Os.fcntlInt(control[0].getFileDescriptor(), android.system.OsConstants.F_SETFL,
            android.system.Os.fcntlInt(control[0].getFileDescriptor(), android.system.OsConstants.F_GETFL, 0) | android.system.OsConstants.O_NONBLOCK);
        android.system.Os.setsockoptTimeval(control[1].getFileDescriptor(), android.system.OsConstants.SOL_SOCKET,
            android.system.OsConstants.SO_SNDTIMEO, android.system.StructTimeval.fromMillis(5000));
        android.os.ParcelFileDescriptor moduleFd = android.os.ParcelFileDescriptor.open(module, android.os.ParcelFileDescriptor.MODE_READ_ONLY);
        CountDownLatch arrived = new CountDownLatch(1), identity = new CountDownLatch(1), died = new CountDownLatch(1);
        AtomicReference<IBinder> binder = new AtomicReference<>();
        AtomicReference<String> failure = new AtomicReference<>();
        AtomicBoolean bound = new AtomicBoolean();
        java.util.concurrent.atomic.AtomicInteger exitCode = new java.util.concurrent.atomic.AtomicInteger(-1);
        android.os.Messenger inbox = new android.os.Messenger(new android.os.Handler(android.os.Looper.getMainLooper(), message -> {
            if (message.what == ModuleBridge.IDENTITY) {
                if (message.sendingUid == android.os.Process.myUid() || message.sendingUid != message.arg2) failure.set("worker has application UID");
                identity.countDown();
            }
            if (message.what == ModuleBridge.EXIT) exitCode.set(message.arg1);
            return true;
        }));
        ServiceConnection connection = new ServiceConnection() {
            @Override public void onServiceConnected(ComponentName name, IBinder service) {
                binder.set(service);
                try { service.linkToDeath(died::countDown, 0); }
                catch (android.os.RemoteException dead) { failure.set("worker died before bootstrap"); }
                arrived.countDown();
            }
            @Override public void onNullBinding(ComponentName name) { failure.set("worker rejected binding"); arrived.countDown(); }
            @Override public void onServiceDisconnected(ComponentName name) { died.countDown(); }
        };
        try {
            mark("binding-worker");
            getInstrumentation().runOnMainSync(() -> {
                Intent intent = new Intent(context, WasmPluginService.class).putExtra("generation", 124L);
                bound.set(context.bindIsolatedService(intent, Context.BIND_AUTO_CREATE | Context.BIND_IMPORTANT,
                    "descriptor_test_" + android.os.SystemClock.elapsedRealtime(), context.getMainExecutor(), connection));
            });
            assertTrue(bound.get());
            assertTrue(arrived.await(10, TimeUnit.SECONDS));
            assertNull(failure.get(), failure.get());
            mark("worker-bound");
            android.os.Message attach = android.os.Message.obtain(null, ModuleBridge.ATTACH);
            attach.replyTo = inbox;
            attach.getData().putLong("generation", 124L);
            attach.getData().putParcelable("module", moduleFd);
            attach.getData().putParcelable("control", control[1]);
            attach.getData().putString("token", "a".repeat(64));
            new android.os.Messenger(binder.get()).send(attach);
            mark("bootstrap-sent");
            assertTrue("worker did not identify its isolated UID", identity.await(5, TimeUnit.SECONDS));
            assertNull(failure.get(), failure.get());
            mark("worker-identity-received");
            org.json.JSONObject hello;
            try { hello = readFrame(control[0].getFileDescriptor()); }
            catch (java.io.IOException error) { throw new java.io.IOException("no Hello; native bootstrap stage=" + exitCode.get(), error); }
            assertEquals("a".repeat(64), hello.getString("token"));
            mark("hello-received");
            byte[] call = "{\"type\":\"call\",\"ticket\":7,\"caller\":\"host\",\"service\":\"echo\",\"version\":1,\"method\":\"echo\",\"args\":null}".getBytes(java.nio.charset.StandardCharsets.UTF_8);
            byte[] frame = java.nio.ByteBuffer.allocate(4 + call.length).putInt(call.length).put(call).array();
            transfer(control[0].getFileDescriptor(), frame, true);
            org.json.JSONObject reply = readFrame(control[0].getFileDescriptor());
            assertEquals("reply", reply.getString("op"));
            assertEquals(7L, reply.getLong("ticket"));
            assertEquals("portable echo", reply.getJSONObject("result").getString("value"));
            mark("sdk-action-completed");
            getInstrumentation().runOnMainSync(() -> context.unbindService(connection));
            bound.set(false);
            assertTrue("isolated worker survived removal of its last binding", died.await(5, TimeUnit.SECONDS));
        } finally {
            if (bound.get()) getInstrumentation().runOnMainSync(() -> context.unbindService(connection));
            moduleFd.close(); control[0].close(); control[1].close();
            assertTrue(module.delete());
        }
    }

    public void testOwnerBridgeTransfersAuthenticatedDataWithoutChangingWorkerSocketOptions() throws Exception {
        System.loadLibrary("peersus_android");
        Context context = getInstrumentation().getTargetContext();
        java.io.File directory = java.nio.file.Files.createTempDirectory(context.getCacheDir().toPath(), "wasm-data-").toFile();
        java.io.File module = new java.io.File(directory, "data.wasm");
        java.io.File controlPath = new java.io.File(directory, "c.sock"), dataPath = new java.io.File(directory, "d.sock");
        long generation = (1L << 60) + android.os.SystemClock.elapsedRealtime();
        try (android.net.LocalSocket controlBinding = new android.net.LocalSocket();
             android.net.LocalSocket dataBinding = new android.net.LocalSocket()) {
            try (java.io.InputStream input = getInstrumentation().getContext().getAssets().open("data.wasm");
                 java.io.FileOutputStream output = new java.io.FileOutputStream(module)) { input.transferTo(output); }
            controlBinding.bind(new android.net.LocalSocketAddress(controlPath.getAbsolutePath(), android.net.LocalSocketAddress.Namespace.FILESYSTEM));
            dataBinding.bind(new android.net.LocalSocketAddress(dataPath.getAbsolutePath(), android.net.LocalSocketAddress.Namespace.FILESYSTEM));
            // These wrappers do not own caller-supplied descriptors (public API contract).
            android.net.LocalServerSocket controlServer = new android.net.LocalServerSocket(controlBinding.getFileDescriptor());
            android.net.LocalServerSocket dataServer = new android.net.LocalServerSocket(dataBinding.getFileDescriptor());
            nonblocking(controlServer.getFileDescriptor()); nonblocking(dataServer.getFileDescriptor());
            assertTrue(ModuleBridge.launch(context, generation, module.getAbsolutePath(), controlPath.getAbsolutePath(), dataPath.getAbsolutePath(), "b".repeat(64)));
            readable(controlServer.getFileDescriptor());
            try (android.net.LocalSocket control = controlServer.accept()) {
                nonblocking(control.getFileDescriptor());
                assertEquals("owner-created control connection keeps the owner's UID", android.os.Process.myUid(), control.getPeerCredentials().getUid());
                assertEquals("b".repeat(64), readFrame(control.getFileDescriptor()).getString("token"));
                byte[] call = "{\"type\":\"call\",\"ticket\":7,\"caller\":\"host\",\"service\":\"echo\",\"version\":1,\"method\":\"data\",\"args\":null}".getBytes(java.nio.charset.StandardCharsets.UTF_8);
                transfer(control.getFileDescriptor(), java.nio.ByteBuffer.allocate(4 + call.length).putInt(call.length).put(call).array(), true);
                readable(dataServer.getFileDescriptor());
                try (android.net.LocalSocket data = dataServer.accept()) {
                    nonblocking(data.getFileDescriptor());
                    assertEquals("owner-created data connection keeps the owner's UID", android.os.Process.myUid(), data.getPeerCredentials().getUid());
                    org.json.JSONObject hello = readFrame(data.getFileDescriptor());
                    assertEquals("b".repeat(64), hello.getString("token"));
                    assertEquals(7L, hello.getLong("ticket"));
                    byte[] bytes = new byte[9]; transfer(data.getFileDescriptor(), bytes, false);
                    assertEquals("real data", new String(bytes, java.nio.charset.StandardCharsets.UTF_8));
                    readable(data.getFileDescriptor());
                    assertEquals("guest must half-close before Reply", 0, android.system.Os.read(data.getFileDescriptor(), new byte[1], 0, 1));
                }
                org.json.JSONObject reply = readFrame(control.getFileDescriptor());
                assertEquals(7L, reply.getLong("ticket"));
                assertEquals("real descriptor data", reply.getJSONObject("result").getString("value"));
            }
        } finally {
            ModuleBridge.stop(generation);
            getInstrumentation().waitForIdleSync();
            if (module.exists()) assertTrue(module.delete());
            if (controlPath.exists()) assertTrue(controlPath.delete());
            if (dataPath.exists()) assertTrue(dataPath.delete());
            assertTrue(directory.delete());
        }
    }


    private void mark(String stage) {
        android.os.Bundle status = new android.os.Bundle();
        status.putString("wasmStage", stage);
        getInstrumentation().sendStatus(0, status);
    }

}
