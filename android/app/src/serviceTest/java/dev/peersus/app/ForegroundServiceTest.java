package dev.peersus.app;

import android.app.ActivityManager;
import android.content.Context;
import android.content.Intent;
import androidx.test.core.app.ActivityScenario;
import org.junit.After;
import org.junit.Test;
import static org.junit.Assert.*;
import static androidx.test.platform.app.InstrumentationRegistry.getInstrumentation;
import java.util.concurrent.atomic.AtomicReference;

/** Opt-in physical-device regression: a cancelled start must still meet the foreground deadline. */
public final class ForegroundServiceTest {
    private ActivityScenario<MainActivity> scenario;
    @After public void closeScenario() {
        if (scenario != null) scenario.close();
    }


    @Test
    public void testCancelledStartBeforeMainThreadCreatesService() throws Exception {
        android.os.Bundle args = androidx.test.platform.app.InstrumentationRegistry.getArguments();
        assertEquals("service lifecycle changes require explicit opt-in", "true", args.getString("service_lifecycle"));
        scenario = ActivityScenario.launch(new Intent(Intent.ACTION_MAIN).setClass(getInstrumentation().getTargetContext(), MainActivity.class)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
        java.util.concurrent.atomic.AtomicReference<MainActivity> current = new java.util.concurrent.atomic.AtomicReference<>();
        scenario.onActivity(current::set);
        MainActivity activity = current.get();
        boolean original = activity.getSharedPreferences("background-service", Context.MODE_PRIVATE).getBoolean("enabled", false);
        try {
            TransferService.update(activity, false, false);
            awaitService(activity, false);
            for (int iteration = 0; iteration < 4; iteration++) {
                AtomicReference<Throwable> failure = new AtomicReference<>();
                getInstrumentation().runOnMainSync(() -> {
                    Thread worker = new Thread(() -> {
                        try {
                            // The main thread cannot run Service.onCreate until both requests complete.
                            TransferService.update(activity, true, false);
                            assertNotNull("foreground start was not accepted", service(activity));
                            TransferService.update(activity, false, false);
                        } catch (Throwable error) { failure.set(error); }
                    }, "foreground-start-stop-validation");
                    worker.start();
                    try {
                        worker.join(2000);
                        if (worker.isAlive()) failure.set(new AssertionError("service request worker did not finish"));
                    } catch (InterruptedException interrupted) {
                        Thread.currentThread().interrupt();
                        failure.set(interrupted);
                    }
                });
                if (failure.get() != null) throw new AssertionError("service lifecycle request failed", failure.get());
                awaitService(activity, false);
            }
        } finally {
            TransferService.update(activity, original, false);
            if (original) awaitService(activity, true);
        }
    }

    // The platform still exposes only this deprecated API for observing our OWN
    // foreground service; no other application's service or identity is inspected.
    @SuppressWarnings("deprecation")
    private static ActivityManager.RunningServiceInfo service(MainActivity activity) {
        for (ActivityManager.RunningServiceInfo item : ((ActivityManager)activity.getSystemService(Context.ACTIVITY_SERVICE)).getRunningServices(128)) {
            if (TransferService.class.getName().equals(item.service.getClassName())) return item;
        }
        return null;
    }

    private void awaitService(MainActivity activity, boolean foreground) throws Exception {
        long deadline = android.os.SystemClock.elapsedRealtime() + 10000;
        do {
            ActivityManager.RunningServiceInfo item = service(activity);
            if (foreground ? item != null && item.foreground : item == null) return;
            Thread.sleep(25);
        } while (android.os.SystemClock.elapsedRealtime() < deadline);
        fail(foreground ? "service never became foreground" : "service did not stop");
    }
}
