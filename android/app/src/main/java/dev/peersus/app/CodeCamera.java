package dev.peersus.app;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Size;
import java.nio.ByteBuffer;
import java.util.Collections;

/**
 * The camera, for reading the code of another device. It only gives frames: the brightness plane of each, which is what
 * a code is read from, handed to Rust with {@link #cameraFrame}. It decides nothing about what is in them, when to stop,
 * or what a code means; all of that is Rust's, and is tried without a phone there.
 *
 * <p>It is the platform's own Camera2, which needs no library besides this program's own. Everything runs on a thread
 * of its own, so that the interface is never held up by a camera.
 */
final class CodeCamera {
    static {
        // The runtime binds a native method through the class that declares it, so this class loads the library itself,
        // as the activity does: the same library, found already loaded, bound to this class before the first call.
        System.loadLibrary("peersus_android");
    }

    /**
     * The size asked for. The code is read from the frame, not from the picture shown, and the code of a pairing is large (about 130 modules
     * across): at 1280 by 720 the circle of it held to the lens has about five pixels to a module, which a little blur turns into nothing.
     * At 1920 by 1080 it has about eight. A frame this size is still cheap to pass: two megabytes of brightness, thirty times a second.
     */
    private static final int WIDTH = 1920;
    private static final int HEIGHT = 1080;

    private final Activity activity;
    private HandlerThread thread;
    private Handler handler;
    private CameraDevice camera;
    private CameraCaptureSession session;
    private ImageReader reader;
    private boolean running;
    // The physical camera asked for inside the logical one that is opened, or null when the logical one is used as it is.
    private String physicalId;
    // What the camera in use says of itself, read when it is opened and used when its request is made.
    private String cameraId;
    private android.graphics.Rect sensorArea;
    private int regionsAe;
    private int regionsAf;
    private CameraManager cameras;

    CodeCamera(Activity activity) {
        this.activity = activity;
    }

    /** Whether the application may use the camera now. */
    boolean allowed() {
        return activity.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED;
    }

    /** Whether the camera is open or opening now. */
    synchronized boolean running() {
        return running;
    }

    /** Start giving frames, if the permission is held. Does nothing when it is not, or when already running. */
    synchronized void start() {
        if (running || !allowed()) return;
        running = true;
        thread = new HandlerThread("code-camera");
        thread.start();
        handler = new Handler(thread.getLooper());
        try {
            CameraManager manager = (CameraManager) activity.getSystemService(Context.CAMERA_SERVICE);
            String choice = backCamera(manager);
            if (choice == null) {
                stop();
                return;
            }
            // A choice is a camera of the phone, or one of the cameras that a camera of the phone is made of, written logical/physical:
            // Android lists only the logical ones, and a wide lens or a long one lives under the main one as a physical camera.
            int slash = choice.indexOf('/');
            String id = slash < 0 ? choice : choice.substring(0, slash);
            physicalId = slash < 0 ? null : choice.substring(slash + 1);
            String traitsId = physicalId != null ? physicalId : id;
            Size size = sizeFor(manager, id);
            CameraCharacteristics traits = manager.getCameraCharacteristics(traitsId);
            cameras = manager;
            cameraId = traitsId;
            sensorArea = traits.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE);
            Integer ae = traits.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AE);
            Integer af = traits.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AF);
            regionsAe = ae == null ? 0 : ae;
            regionsAf = af == null ? 0 : af;
            Integer sensor = traits.get(CameraCharacteristics.SENSOR_ORIENTATION);
            if (sensor != null) sensorDegrees = sensor;
            reader = ImageReader.newInstance(size.getWidth(), size.getHeight(), ImageFormat.YUV_420_888, 2);
            reader.setOnImageAvailableListener(this::onImage, handler);
            // The permission was checked just above; a revocation in between is a failed open, which is handled.
            manager.openCamera(id, new CameraDevice.StateCallback() {
                @Override public void onOpened(CameraDevice device) { opened(device); }
                @Override public void onDisconnected(CameraDevice device) { device.close(); stop(); }
                @Override public void onError(CameraDevice device, int error) { device.close(); stop(); }
            }, handler);
        } catch (CameraAccessException | SecurityException unavailable) {
            stop();
        }
    }

    /** Stop, and let go of the camera and its thread. Safe to call at any time, and more than once. */
    synchronized void stop() {
        running = false;
        if (session != null) { try { session.close(); } catch (Exception ignored) { } session = null; }
        if (camera != null) { try { camera.close(); } catch (Exception ignored) { } camera = null; }
        if (reader != null) { reader.close(); reader = null; }
        if (thread != null) { thread.quitSafely(); thread = null; handler = null; }
    }

    private void opened(CameraDevice device) {
        synchronized (this) {
            if (!running) { device.close(); return; }
            camera = device;
        }
        try {
            CameraCaptureSession.StateCallback callback = new CameraCaptureSession.StateCallback() {
                @Override public void onConfigured(CameraCaptureSession configured) { configured(configured, device); }
                @Override public void onConfigureFailed(CameraCaptureSession failed) { stop(); }
            };
            if (physicalId != null && android.os.Build.VERSION.SDK_INT >= 28) {
                android.hardware.camera2.params.OutputConfiguration output = new android.hardware.camera2.params.OutputConfiguration(reader.getSurface());
                output.setPhysicalCameraId(physicalId);
                device.createCaptureSession(new android.hardware.camera2.params.SessionConfiguration(
                        android.hardware.camera2.params.SessionConfiguration.SESSION_REGULAR, Collections.singletonList(output),
                        command -> handler.post(command), callback));
            } else {
                device.createCaptureSession(Collections.singletonList(reader.getSurface()), callback, handler);
            }
        } catch (CameraAccessException | IllegalStateException failed) {
            stop();
        }
    }

    private void configured(CameraCaptureSession configured, CameraDevice device) {
        synchronized (this) {
            if (!running) { configured.close(); return; }
            session = configured;
        }
        try {
            CaptureRequest.Builder request = device.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
            request.addTarget(reader.getSurface());
            request.set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO);
            request.set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON);
            request.set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_AUTO);
            // Left to the driver, the frame rate settled on 10 to 15 a second (measured on a real phone: 11 frames a second), and a
            // code held in front of the camera stuttered. The range is asked for outright, from the ones this camera says it offers.
            android.util.Range<Integer> rate = fastestRate(cameraId);
            if (rate != null) request.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, rate);
            // A code is shown on a screen, which is a light and not a lit scene: the exposure the camera chose for it was measured at a mean
            // of 16 and a brightest point of 40 out of 255, a picture a decoder cannot tell black from white in. A step of exposure up is
            // asked for, as much as the camera allows up to one stop (its steps are counted in the units it says).
            android.util.Range<Integer> span = cameras.getCameraCharacteristics(cameraId).get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE);
            android.util.Rational unit = cameras.getCameraCharacteristics(cameraId).get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP);
            if (span != null && unit != null && unit.floatValue() > 0) {
                int stop = Math.round(1f / unit.floatValue());
                request.set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, Math.max(span.getLower(), Math.min(span.getUpper(), stop)));
            }
            // A code is held a hand's length from the lens, and the middle of the picture is where it is: exposure and focus are
            // measured there, and not over a whole scene a bright window beside it can fool.
            android.graphics.Rect area = sensorArea;
            if (area != null) {
                int w = area.width() / 3, h = area.height() / 3;
                android.hardware.camera2.params.MeteringRectangle centre = new android.hardware.camera2.params.MeteringRectangle(
                        area.centerX() - w / 2, area.centerY() - h / 2, w, h, android.hardware.camera2.params.MeteringRectangle.METERING_WEIGHT_MAX);
                if (regionsAe > 0) request.set(CaptureRequest.CONTROL_AE_REGIONS, new android.hardware.camera2.params.MeteringRectangle[] {centre});
                if (regionsAf > 0) request.set(CaptureRequest.CONTROL_AF_REGIONS, new android.hardware.camera2.params.MeteringRectangle[] {centre});
            }
            // A code is held a hand's length from the lens. The continuous mode for pictures is made for far scenes, and was measured here
            // settling on "not focused, locked" on a near screen: every part of the picture came out blurred. Macro is made for this
            // distance, and the camera is asked for it when it lists it; auto, which the trigger below drives, when it does not.
            int focus = focusModeFor(cameraId);
            request.set(CaptureRequest.CONTROL_AF_MODE, focus);
            android.util.Log.i("PeersusCam", "asked: rate " + rate + ", focus mode " + focus + ", regions ae " + regionsAe + " af " + regionsAf);
            CaptureRequest.Builder search = device.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
            search.addTarget(reader.getSurface());
            search.set(CaptureRequest.CONTROL_AF_MODE, focus);
            if (rate != null) search.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, rate);
            search.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_START);
            final CaptureRequest triggered = search.build();
            // What the camera says of its focus with each result. While it is not focused, a new search is started every second and a half:
            // a search that ended unfocused is not tried again by the camera on its own, which is what left the picture blurred for minutes.
            configured.setRepeatingRequest(request.build(), new CameraCaptureSession.CaptureCallback() {
                private long lastSearch;
                @Override public void onCaptureCompleted(CameraCaptureSession s, CaptureRequest r, android.hardware.camera2.TotalCaptureResult result) {
                    Integer state = result.get(android.hardware.camera2.CaptureResult.CONTROL_AF_STATE);
                    if (state == null) return;
                    boolean sharp = state == android.hardware.camera2.CaptureResult.CONTROL_AF_STATE_FOCUSED_LOCKED
                            || state == android.hardware.camera2.CaptureResult.CONTROL_AF_STATE_PASSIVE_FOCUSED;
                    long now = android.os.SystemClock.elapsedRealtime();
                    if (!sharp && state != android.hardware.camera2.CaptureResult.CONTROL_AF_STATE_ACTIVE_SCAN
                            && state != android.hardware.camera2.CaptureResult.CONTROL_AF_STATE_PASSIVE_SCAN && now - lastSearch > 1500) {
                        lastSearch = now;
                        try { s.capture(triggered, null, handler); } catch (CameraAccessException | IllegalStateException ignored) { }
                    }
                }
            }, handler);
            configured.capture(triggered, null, handler);
        } catch (CameraAccessException | IllegalStateException failed) {
            stop();
        }
    }

    /**
     * The focus mode for a code held at a hand's length: auto, which the trigger below drives to the real distance of the code. Macro was
     * measured staying locked at ten centimetres on a real phone (focus distance 10 dioptres, "focused, locked"): sharp for a screen
     * touching the lens and blurred for one held at the distance a person holds a phone. Macro is only the answer when auto is not offered.
     */
    private int focusModeFor(String id) throws CameraAccessException {
        int[] modes = cameras.getCameraCharacteristics(id).get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES);
        if (modes != null) {
            for (int mode : modes) if (mode == CaptureRequest.CONTROL_AF_MODE_AUTO) return mode;
            for (int mode : modes) if (mode == CaptureRequest.CONTROL_AF_MODE_MACRO) return mode;
        }
        return CaptureRequest.CONTROL_AF_MODE_AUTO;
    }

    /** One frame: its brightness plane, copied out row by row without the padding at the end of each, then handed to Rust. */
    private void onImage(ImageReader source) {
        try (Image image = source.acquireLatestImage()) {
            if (image == null) return;
            Image.Plane plane = image.getPlanes()[0];
            int width = image.getWidth(), height = image.getHeight();
            int stride = plane.getRowStride();
            ByteBuffer buffer = plane.getBuffer();
            byte[] luma = new byte[width * height];
            // A row on the camera's side can be longer than the picture it holds (padding): only the picture is copied,
            // since Rust is told the width and reads that many bytes to a row.
            for (int row = 0; row < height; row++) {
                buffer.position(row * stride);
                buffer.get(luma, row * width, width);
            }
            measure(luma, width, height);
            sayTurns();
            cameraFrame(luma, width, height);
        } catch (RuntimeException skipped) {
            // A frame that could not be read is dropped; the next one is tried.
        }
    }

    private long measuredAt;
    private int framesSince;

    /**
     * What the camera really gives, said to logcat once a second so that a scan that finds nothing can be told apart: no
     * frames at all, frames that are dark or flat, or frames that are fine and hold no code. Reads a sample of the plane
     * and changes nothing about what is scanned.
     */
    private void measure(byte[] luma, int width, int height) {
        framesSince++;
        long now = android.os.SystemClock.elapsedRealtime();
        if (now - measuredAt < 1000) return;
        // A grid of about ten thousand points over the whole frame, and another over the square at its centre, which is the part the
        // circle shows: a code in the circle is bright there whatever the rest of the frame holds.
        int step = Math.max(1, Math.min(width, height) / 100);
        long sum = 0, centreSum = 0;
        int low = 255, high = 0, count = 0, centreLow = 255, centreHigh = 0, centreCount = 0;
        int side = Math.min(width, height), left = (width - side) / 2, top = (height - side) / 2;
        for (int y = 0; y < height; y += step) {
            for (int x = 0; x < width; x += step) {
                int value = luma[y * width + x] & 0xff;
                sum += value;
                if (value < low) low = value;
                if (value > high) high = value;
                count++;
                if (x >= left && x < left + side && y >= top && y < top + side) {
                    centreSum += value;
                    if (value < centreLow) centreLow = value;
                    if (value > centreHigh) centreHigh = value;
                    centreCount++;
                }
            }
        }
        android.util.Log.i("PeersusCam", framesSince + " frames in " + (now - measuredAt) + " ms, " + width + "x" + height
                + ", whole mean " + (count == 0 ? 0 : sum / count) + " (" + low + ".." + high + ")"
                + ", centre mean " + (centreCount == 0 ? 0 : centreSum / centreCount) + " (" + centreLow + ".." + centreHigh + ")");
        measuredAt = now;
        framesSince = 0;
    }

    /** The angle the sensor of the camera in use is mounted at, in degrees; 90 on nearly every phone's back camera. */
    private volatile int sensorDegrees = 90;

    /** Say how many quarter turns the picture needs now. Cheap: one angle kept and one the screen gives. */
    private void sayTurns() {
        int screen = 0;
        try {
            android.view.Display display = activity.getDisplay();
            if (display != null) screen = display.getRotation() * 90;
        } catch (RuntimeException unknown) {
            // The screen's turn is not known: the picture is then shown as the sensor has it, never not at all.
        }
        int degrees = ((sensorDegrees - screen) % 360 + 360) % 360;
        cameraTurns(degrees / 90);
    }

    /** The name the choice of the person is kept under, in the preferences called {@link #PREFERENCES}: the id of a camera, or nothing. */
    static final String CHOICE = "choice";
    static final String PREFERENCES = "camera";

    /**
     * The camera in use: the one the person chose if this phone still has it as a back camera. When nothing was chosen, the main one: of the
     * back cameras, the one whose focal length is that of a main lens (four to eight millimetres, which leaves out the wide and the long
     * ones) and which focuses the nearest, since a code is held close; the first back camera when none is like that. The first one alone was
     * measured to be the wrong thing to take on a phone that lists a wide or a long lens before its main one.
     */
    private String backCamera(CameraManager manager) throws CameraAccessException {
        String chosen = activity.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(CHOICE, "");
        String first = null, main = null;
        float nearestMain = Float.MAX_VALUE;
        for (String id : manager.getCameraIdList()) {
            CameraCharacteristics traits = manager.getCameraCharacteristics(id);
            Integer facing = traits.get(CameraCharacteristics.LENS_FACING);
            if (facing == null || facing != CameraCharacteristics.LENS_FACING_BACK) continue;
            if (id.equals(chosen)) return id;
            if (first == null) first = id;
            if (android.os.Build.VERSION.SDK_INT >= 28) {
                for (String physical : traits.getPhysicalCameraIds()) {
                    if ((id + "/" + physical).equals(chosen)) return chosen;
                }
            }
            float[] lengths = traits.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS);
            Float minimum = traits.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE);
            boolean isMain = lengths != null && lengths.length > 0 && lengths[0] >= 4f && lengths[0] < 8f;
            // The minimum focus distance is in dioptres: the greater it is, the nearer the camera can focus. A camera that cannot focus (0) is not
            // preferred over one that can.
            float nearness = minimum == null ? 0f : minimum;
            if (isMain && nearness > 0f && (main == null || 1f / nearness < nearestMain)) {
                main = id;
                nearestMain = 1f / nearness;
            }
        }
        if (chosen.isEmpty() || !chosen.contains("/")) return main != null ? main : first;
        return main != null ? main : first;
    }

    /**
     * The back cameras of this phone, one line each, for the person to choose from: the id, what the lens is (its focal length in
     * millimetres, which tells a wide lens from a main one from a long one), the nearest it can focus on in centimetres (none when it
     * cannot focus), and whether it is the one in use. Lines are tab separated, in the order the phone lists them.
     */
    static String describeBackCameras(Activity activity) {
        StringBuilder lines = new StringBuilder();
        try {
            CameraManager manager = (CameraManager) activity.getSystemService(Context.CAMERA_SERVICE);
            String chosen = activity.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(CHOICE, "");
            for (String id : manager.getCameraIdList()) {
                CameraCharacteristics traits = manager.getCameraCharacteristics(id);
                Integer facing = traits.get(CameraCharacteristics.LENS_FACING);
                if (facing == null || facing != CameraCharacteristics.LENS_FACING_BACK) continue;
                describeOne(lines, id, id, traits, chosen);
                if (android.os.Build.VERSION.SDK_INT >= 28) {
                    for (String physical : traits.getPhysicalCameraIds()) {
                        describeOne(lines, id + "/" + physical, physical, manager.getCameraCharacteristics(physical), chosen);
                    }
                }
            }
        } catch (CameraAccessException | RuntimeException unavailable) {
            // No list is no choice: Settings shows nothing to choose, and the camera is the first back one as before.
        }
        return lines.toString();
    }

    /** One line of the list: the id the choice is written with, then what the lens is, how near it focuses, and whether it is the chosen one. */
    private static void describeOne(StringBuilder lines, String choice, String shown, CameraCharacteristics traits, String chosen) {
        float[] lengths = traits.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS);
        Float nearest = traits.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE);
        int centimetres = nearest == null || nearest <= 0 ? 0 : Math.round(100f / nearest);
        if (lines.length() > 0) lines.append('\n');
        lines.append(choice).append('\t').append(lengths == null || lengths.length == 0 ? 0 : Math.round(lengths[0] * 10)).append('\t')
                .append(centimetres).append('\t').append(choice.equals(chosen) ? 1 : 0);
    }

    /**
     * The steadiest fast frame rate this camera offers: the range whose lower end is highest, and of those the one whose upper end is
     * highest. A range such as 30 to 30 holds the rate, where 10 to 30 lets the exposure take frames away in dim light, which is the
     * stutter of a code held in front of the lens. Null when the camera lists none, and it is then left to the driver.
     */
    private android.util.Range<Integer> fastestRate(String id) throws CameraAccessException {
        android.util.Range<Integer>[] ranges = cameras.getCameraCharacteristics(id).get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES);
        if (ranges == null || ranges.length == 0) return null;
        android.util.Range<Integer> best = null;
        for (android.util.Range<Integer> range : ranges) {
            if (best == null || range.getLower() > best.getLower()
                    || (range.getLower().equals(best.getLower()) && range.getUpper() > best.getUpper())) best = range;
        }
        return best;
    }

    /** The smallest size the camera offers that is at least as large as the one asked for, else the largest it offers. */
    private static Size sizeFor(CameraManager manager, String id) throws CameraAccessException {
        StreamConfigurationMap map = manager.getCameraCharacteristics(id).get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
        Size[] sizes = map == null ? null : map.getOutputSizes(ImageFormat.YUV_420_888);
        if (sizes == null || sizes.length == 0) return new Size(WIDTH, HEIGHT);
        Size best = null;
        for (Size size : sizes) {
            boolean enough = size.getWidth() >= WIDTH && size.getHeight() >= HEIGHT;
            if (enough && (best == null || size.getWidth() * size.getHeight() < best.getWidth() * best.getHeight())) best = size;
        }
        if (best != null) return best;
        for (Size size : sizes) if (best == null || size.getWidth() * size.getHeight() > best.getWidth() * best.getHeight()) best = size;
        return best;
    }

    /** One frame of the camera: its brightness plane, one byte a pixel, {@code width} bytes to a row. Rust's. */
    private static native void cameraFrame(byte[] luma, int width, int height);

    /**
     * How many quarter turns clockwise the picture must be turned to stand upright, said to Rust. The rule is the
     * platform's own for a back camera (the sensor's mounting minus the screen's turn), and Rust holds the same rule,
     * tried there; this only reads the two angles and says the result.
     */
    private static native void cameraTurns(int turns);
}
