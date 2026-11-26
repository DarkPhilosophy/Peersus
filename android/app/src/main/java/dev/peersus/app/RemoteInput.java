package dev.peersus.app;

import android.accessibilityservice.AccessibilityService;
import android.accessibilityservice.GestureDescription;
import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.PixelFormat;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.DisplayMetrics;
import android.view.Gravity;
import android.view.View;
import android.view.WindowManager;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;

import org.json.JSONArray;
import org.json.JSONObject;

/**
 * What a paired device does on this phone, replayed through the system's accessibility service: a pointer of its own
 * drawn over everything, moved by the other device's touchpad or mouse, a press of its left button a tap where it is,
 * a press held and moved a drag, the wheel a swipe, keys and text written into the focused field. The person turns
 * the service on by hand in the system's settings; until then nothing is replayed and the caller is told why.
 *
 * Android gives no application a mouse of its own, so this is the only way to act on another application's window,
 * the same one assistive pointers use. Nothing is read from the screen but the focused field, and only to write to it.
 */
public final class RemoteInput extends AccessibilityService {
    private static volatile RemoteInput running;

    static boolean connected() { return running != null; }

    static boolean approved(Context context) {
        String enabled = android.provider.Settings.Secure.getString(context.getContentResolver(),
            android.provider.Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES);
        android.content.ComponentName own = new android.content.ComponentName(context, RemoteInput.class);
        if (enabled == null || enabled.length() > 65536) return false;
        for (String value : enabled.split(":")) {
            if (own.equals(android.content.ComponentName.unflattenFromString(value))) return true;
        }
        return false;
    }
    private final Handler main = new Handler(Looper.getMainLooper());
    private final ReplayEpoch inputEpoch = new ReplayEpoch();
    private View pointer;
    private float x = -1, y = -1;
    /** Where the left button went down, or null: a press that moved before it let go is a drag, otherwise a tap. */
    private Path drag;
    private long downAt;
    private static final int SIZE = 44;

    /** Queue admission and retirement share the same lock as each replay side effect. */
    static final class ReplayEpoch {
        private volatile long value;

        synchronized void enqueue(java.util.function.Consumer<Runnable> post,
                                  java.util.function.LongConsumer replay) {
            long ticket = value;
            post.accept(() -> { if (current(ticket)) replay.accept(ticket); });
        }

        synchronized void fence(java.util.function.Consumer<Runnable> post, Runnable cleanup) {
            ++value;
            post.accept(cleanup);
        }

        boolean current(long ticket) { return ticket == value; }
    }

    @Override protected void onServiceConnected() {
        running = this;
    }

    @Override public boolean onUnbind(android.content.Intent intent) {
        if (running == this) running = null;
        main.post(this::hide);
        InputRecoveryProvider.request();
        return super.onUnbind(intent);
    }

    @Override public void onDestroy() {
        if (running == this) running = null;
        main.removeCallbacks(idle);
        hide();
        super.onDestroy();
    }

    @Override public void onAccessibilityEvent(AccessibilityEvent event) { }

    @Override public void onInterrupt() { }

    /** The screen as a layout places it, whether or not the service is on: a layout shows the real size of this phone. */
    public static String placeable(Context context) { return screen(context); }

    /** The screen in pixels, "width height": what a layout places this phone as. Works without the service. */
    public static String screen(Context context) {
        DisplayMetrics metrics = context.getResources().getDisplayMetrics();
        WindowManager windows = context.getSystemService(WindowManager.class);
        if (windows != null && android.os.Build.VERSION.SDK_INT >= 30) {
            android.graphics.Rect bounds = windows.getMaximumWindowMetrics().getBounds();
            return bounds.width() + " " + bounds.height();
        }
        return metrics.widthPixels + " " + metrics.heightPixels;
    }

    /** Replay a batch of input. Null when it was replayed, otherwise why it was not, in the application's own words. */
    public static String inject(Context context, String batch) {
        RemoteInput service = running;
        if (service == null) {
            if ("[]".equals(batch)) return null;
            InputRecoveryProvider.request();
            String words = "Turn on Peersus remote control in the phone's accessibility settings to let a paired device control this phone";
            tell(context, words);
            return words;
        }
        final JSONArray events;
        try {
            events = new JSONArray(batch);
        } catch (Exception error) {
            return "The input was not understood";
        }
        if (events.length() == 0) return "The input was not understood";
        service.inputEpoch.enqueue(service.main::post, epoch -> service.replay(events, epoch));
        return null;
    }

    /** Release held input; a resting gesture keeps its pointer, leaving takes it away immediately. */
    public static void release(boolean keep) {
        RemoteInput service = running;
        if (service == null) return;
        // Fence the old Java work synchronously, even for an ordinary release.
        // Later batches are posted after this cleanup under the same lock.
        service.inputEpoch.fence(service.main::post, () -> {
            service.drag = null;
            if (keep) {
                service.idleLater();
            } else {
                service.main.removeCallbacks(service.idle);
                service.hide();
            }
        });
    }

    /** How long a pointer nobody moves stays on the screen. */
    private static final long IDLE_HIDE_MS = 3_000;
    private final Runnable idle = () -> { if (drag == null) hide(); };

    /** Take the pointer away once nothing has moved it for a while; a held drag keeps it. */
    private void idleLater() {
        main.removeCallbacks(idle);
        main.postDelayed(idle, IDLE_HIDE_MS);
    }

    private static long toldAt;

    /** Said on the phone itself, at most every half minute: the device that tries to control it cannot fix this. */
    private static void tell(Context context, String words) {
        long now = android.os.SystemClock.elapsedRealtime();
        if (now - toldAt < 30_000) return;
        toldAt = now;
        new Handler(Looper.getMainLooper()).post(() ->
            android.widget.Toast.makeText(context.getApplicationContext(), words, android.widget.Toast.LENGTH_LONG).show());
    }

    private void replay(JSONArray events, long epoch) {
        String[] size = screen(this).split(" ");
        float width = Float.parseFloat(size[0]), height = Float.parseFloat(size[1]);
        if (x < 0) { x = width / 2; y = height / 2; }
        for (int i = 0; i < events.length(); i++) {
            JSONObject event = events.optJSONObject(i);
            if (event == null) continue;
            // Serialize the epoch cut-off with each side effect, not with the whole batch.
            synchronized (inputEpoch) {
                if (!inputEpoch.current(epoch)) return;
                switch (event.optString("t")) {
                    case "move":
                        x = clamp(x + (float) event.optDouble("dx", 0), width);
                        y = clamp(y + (float) event.optDouble("dy", 0), height);
                        if (drag != null) drag.lineTo(x, y);
                        break;
                    case "position":
                        x = clamp((float) event.optDouble("x", 0) * width, width);
                        y = clamp((float) event.optDouble("y", 0) * height, height);
                        if (drag != null) drag.lineTo(x, y);
                        break;
                    case "button":
                        button(event.optString("button"), event.optBoolean("down"));
                        break;
                    case "scroll":
                        scroll(event.optInt("dx"), event.optInt("dy"), height);
                        break;
                    case "key":
                        key(event.optString("key"));
                        break;
                    case "text":
                        write(event.optString("text"), false);
                        break;
                    default:
                        break;
                }
            }
        }
        synchronized (inputEpoch) {
            if (inputEpoch.current(epoch)) show();
        }
    }

    private static float clamp(float value, float most) {
        return Math.max(0, Math.min(most - 1, value));
    }

    private void button(String which, boolean down) {
        if ("left".equals(which)) {
            if (down) {
                drag = new Path();
                drag.moveTo(x, y);
                downAt = android.os.SystemClock.uptimeMillis();
            } else if (drag != null) {
                Path stroke = drag;
                drag = null;
                long held = Math.max(1, Math.min(android.os.SystemClock.uptimeMillis() - downAt, 10_000));
                // A path that never left the point it began at is a tap there.
                android.graphics.PathMeasure measure = new android.graphics.PathMeasure(stroke, false);
                if (measure.getLength() < 1) {
                    stroke = new Path();
                    stroke.moveTo(x, y);
                    held = Math.min(held, 200);
                }
                gesture(stroke, held);
            }
        } else if (!down && "right".equals(which)) {
            performGlobalAction(GLOBAL_ACTION_BACK);
        } else if (!down && "middle".equals(which)) {
            performGlobalAction(GLOBAL_ACTION_HOME);
        }
    }

    private void scroll(int dx, int dy, float height) {
        if (dx == 0 && dy == 0) return;
        float step = height / 12;
        Path path = new Path();
        path.moveTo(x, y);
        // Down on a wheel shows what is below, as a finger moving up does.
        path.lineTo(clamp(x - dx * step, Float.MAX_VALUE), clamp(y - dy * step, height));
        gesture(path, 150);
    }

    private void gesture(Path path, long duration) {
        try {
            dispatchGesture(new GestureDescription.Builder()
                .addStroke(new GestureDescription.StrokeDescription(path, 0, duration))
                .build(), null, null);
        } catch (IllegalArgumentException ignored) {
            // A stroke the system refuses (off the screen, too long) is not replayed.
        }
    }

    private void key(String key) {
        switch (key) {
            case "enter": {
                AccessibilityNodeInfo field = focused();
                if (field != null && android.os.Build.VERSION.SDK_INT >= 30) {
                    field.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.getId());
                } else {
                    write("\n", false);
                }
                break;
            }
            case "backspace":
                write("", true);
                break;
            case "escape":
                performGlobalAction(GLOBAL_ACTION_BACK);
                break;
            default:
                break;
        }
    }

    /** Add `text` to the focused field, or take away its last character. */
    private void write(String text, boolean erase) {
        AccessibilityNodeInfo field = focused();
        if (field == null) return;
        CharSequence now = field.getText();
        String current = now == null || field.isShowingHintText() ? "" : now.toString();
        String next = erase
            ? current.isEmpty() ? current : current.substring(0, current.offsetByCodePoints(current.length(), -1))
            : current + text;
        Bundle arguments = new Bundle();
        arguments.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, next);
        field.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, arguments);
    }

    private AccessibilityNodeInfo focused() {
        AccessibilityNodeInfo root = getRootInActiveWindow();
        return root == null ? null : root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT);
    }

    private void show() {
        WindowManager windows = getSystemService(WindowManager.class);
        if (windows == null) return;
        WindowManager.LayoutParams place = new WindowManager.LayoutParams(SIZE, SIZE,
            WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE | WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE
                | WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN | WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS,
            PixelFormat.TRANSLUCENT);
        place.gravity = Gravity.TOP | Gravity.START;
        place.x = (int) x;
        place.y = (int) y;
        if (pointer == null) {
            pointer = new Arrow(this);
            windows.addView(pointer, place);
        } else {
            windows.updateViewLayout(pointer, place);
        }
        idleLater();
    }

    private void hide() {
        if (pointer == null) return;
        WindowManager windows = getSystemService(WindowManager.class);
        if (windows != null) windows.removeView(pointer);
        pointer = null;
    }

    /** The pointer: an arrow with its tip at the top left, where it acts, dark with a light edge so it reads on anything. */
    private static final class Arrow extends View {
        private final Paint fill = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Paint edge = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Path shape = new Path();

        Arrow(Context context) {
            super(context);
            fill.setColor(Color.rgb(20, 20, 20));
            edge.setColor(Color.WHITE);
            edge.setStyle(Paint.Style.STROKE);
            edge.setStrokeWidth(3);
            float s = SIZE;
            shape.moveTo(2, 2);
            shape.lineTo(2, s * 0.85f);
            shape.lineTo(s * 0.3f, s * 0.62f);
            shape.lineTo(s * 0.5f, s - 2);
            shape.lineTo(s * 0.62f, s * 0.94f);
            shape.lineTo(s * 0.44f, s * 0.58f);
            shape.lineTo(s * 0.78f, s * 0.58f);
            shape.close();
        }

        @Override protected void onDraw(Canvas canvas) {
            canvas.drawPath(shape, fill);
            canvas.drawPath(shape, edge);
        }
    }
}
