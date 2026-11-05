package dev.peersus.app;

import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Message;
import android.os.Messenger;
import android.os.ParcelFileDescriptor;
import android.os.Process;
import android.test.InstrumentationTestCase;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

/** Actual Binder, descriptors and worker death. No network, clipboard or device identity access. */
public final class StoreFetchServiceTest extends InstrumentationTestCase {
    public void testTraversalIsRefusedInASeparateWorkerAndWorkerStops() throws Exception {
        Context context = getInstrumentation().getTargetContext();
        File output = File.createTempFile("store-worker-", ".tmp", context.getCacheDir());
        byte[] original = "unchanged".getBytes(StandardCharsets.US_ASCII);
        Files.write(output.toPath(), original);
        CountDownLatch connected = new CountDownLatch(1), replied = new CountDownLatch(1), died = new CountDownLatch(1);
        AtomicReference<IBinder> binder = new AtomicReference<>();
        AtomicInteger worker = new AtomicInteger(), status = new AtomicInteger(-1), uid = new AtomicInteger(-1);
        AtomicBoolean bound = new AtomicBoolean();
        Messenger inbox = new Messenger(new Handler(Looper.getMainLooper(), message -> {
            if (message.what == StoreFetchService.RESULT) {
                worker.set(message.arg2); status.set(message.arg1); uid.set(message.sendingUid); replied.countDown();
            }
            return true;
        }));
        ServiceConnection connection = new ServiceConnection() {
            @Override public void onServiceConnected(ComponentName component, IBinder service) {
                binder.set(service);
                try { service.linkToDeath(died::countDown, 0); } catch (android.os.RemoteException dead) { died.countDown(); }
                connected.countDown();
            }
            @Override public void onNullBinding(ComponentName component) { connected.countDown(); }
            @Override public void onServiceDisconnected(ComponentName component) { died.countDown(); }
        };
        try {
            getInstrumentation().runOnMainSync(() -> bound.set(context.bindService(
                    new Intent(context, StoreFetchService.class), connection, Context.BIND_AUTO_CREATE)));
            assertTrue(bound.get());
            assertTrue("worker binding timed out", connected.await(10, TimeUnit.SECONDS));
            assertNotNull(binder.get());
            try (ParcelFileDescriptor descriptor = ParcelFileDescriptor.open(output, ParcelFileDescriptor.MODE_WRITE_ONLY)) {
                Message request = Message.obtain(null, StoreFetchService.FETCH);
                request.replyTo = inbox;
                request.getData().putString("name", "../catalog.json");
                request.getData().putLong("limit", 16);
                request.getData().putParcelable("output", descriptor);
                new Messenger(binder.get()).send(request);
            }
            assertTrue("worker refusal timed out", replied.await(5, TimeUnit.SECONDS));
            assertEquals(0, status.get());
            assertEquals(Process.myUid(), uid.get());
            assertTrue(worker.get() > 0 && worker.get() != Process.myPid());
            assertTrue(java.util.Arrays.equals(original, Files.readAllBytes(output.toPath())));
            new Messenger(binder.get()).send(Message.obtain(null, StoreFetchService.STOP));
            assertTrue("worker did not terminate", died.await(5, TimeUnit.SECONDS));
        } finally {
            if (bound.get()) getInstrumentation().runOnMainSync(() -> context.unbindService(connection));
            assertTrue(output.delete());
        }
    }

    public void testBridgeRefusesMainThreadWithoutTouchingTheOutput() throws Exception {
        Context context = getInstrumentation().getTargetContext();
        File output = File.createTempFile("store-main-", ".tmp", context.getCacheDir());
        byte[] original = "untouched".getBytes(StandardCharsets.US_ASCII);
        Files.write(output.toPath(), original);
        AtomicBoolean accepted = new AtomicBoolean(true);
        try {
            getInstrumentation().runOnMainSync(() -> accepted.set(StoreFetchBridge.download(
                    context, "../catalog.json", 16, output.getAbsolutePath())));
            assertFalse(accepted.get());
            assertTrue(java.util.Arrays.equals(original, Files.readAllBytes(output.toPath())));
            assertFalse(StoreFetchBridge.download(context, "../catalog.json", 16, output.getAbsolutePath()));
        } finally {
            assertTrue(output.delete());
        }
    }
}
