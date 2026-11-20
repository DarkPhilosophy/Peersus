package dev.peersus.app;

import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothManager;
import android.bluetooth.le.AdvertiseCallback;
import android.bluetooth.le.AdvertiseData;
import android.bluetooth.le.AdvertiseSettings;
import android.bluetooth.le.AdvertisingSet;
import android.bluetooth.le.AdvertisingSetCallback;
import android.bluetooth.le.AdvertisingSetParameters;
import android.bluetooth.le.BluetoothLeAdvertiser;
import android.bluetooth.le.BluetoothLeScanner;
import android.bluetooth.le.ScanCallback;
import android.bluetooth.le.ScanFilter;
import android.bluetooth.le.ScanRecord;
import android.bluetooth.le.ScanResult;
import android.bluetooth.le.ScanSettings;
import android.content.Context;
import android.content.pm.PackageManager;
import android.os.ParcelUuid;
import android.os.SystemClock;
import android.util.Log;
import java.util.Arrays;

/**
 * Bluetooth Low Energy for finding a paired device that is brought near, and devices nearby to pair with (experimental).
 * It announces the bytes Rust gives it as the data of one of two services, and tells Rust the data of every announcement
 * of those services it hears, with the strength it was heard at. It decides nothing: which device an announcement names,
 * what an offer to pair says and what "near" is are Rust's.
 *
 * Nothing connects and nothing is paired at the Bluetooth level; the system's name and address are not announced. Rust
 * says how things should be again and again, so each call does nothing when it is already so.
 *
 * To see what is heard while measuring: {@code adb shell setprop log.tag.PeersusNearby DEBUG}. Only the kind, the
 * length and the strength are written, never the data.
 */
final class NearbyRadio {
    // The answers of `announce` and `listen`, which Rust reads as numbers.
    static final int OK = 0;
    static final int NO_RADIO = 1;
    static final int OFF = 2;
    static final int NOT_ALLOWED = 3;
    static final int FAILED = 4;

    /** The application's own services; the same values as `peersus_core::nearby::SERVICE` and `OFFER_SERVICE`. */
    private static final ParcelUuid SERVICE = ParcelUuid.fromString("6f1d3c52-8a0b-4e9f-9c57-2b6e41d07a13");
    private static final ParcelUuid OFFER = ParcelUuid.fromString("6e1bcff1-7b65-4c9e-acca-319711224d9c");
    /** The longest data told to Rust of each: more is nobody's, and is not carried. */
    private static final int MOST_TAG = 32;
    private static final int MOST_OFFER = 200;
    /** What an announcement of a service's data takes beside the data: a length, a type and the 128-bit identifier. */
    private static final int AROUND_DATA = 18;
    /** How long after the system refused before it is asked again: a refusal is not answered by asking every second. */
    private static final long AGAIN_AFTER_MS = 30_000;
    /**
     * How long one scan runs before it is stopped and started anew. A scan that has run for long is turned by the system
     * into one that hears only what other applications' scans hear (half an hour in the system's own source). On the
     * phone this was measured on, a scan about half an hour old heard between a quarter and a ninth of what a new one
     * heard of the same announcements.
     */
    private static final long SCAN_ANEW_AFTER_MS = 5 * 60_000;
    private static final String MEASURING = "PeersusNearby";

    static { System.loadLibrary("peersus_android"); }

    private static byte[] announced;
    private static AdvertiseCallback advertising;
    private static long advertiseRefusedAt = -1;
    private static byte[] offered;
    private static AdvertisingSetCallback offering;
    private static long offerRefusedAt = -1;
    private static ScanCallback scanning;
    private static long scanStartedAt = -1;
    private static long scanRefusedAt = -1;

    private NearbyRadio() {}

    /** Whether the person allowed both halves; one permission group, asked for together by the activity. */
    static boolean allowed(Context context) {
        return context.checkSelfPermission(android.Manifest.permission.BLUETOOTH_ADVERTISE) == PackageManager.PERMISSION_GRANTED
                && context.checkSelfPermission(android.Manifest.permission.BLUETOOTH_SCAN) == PackageManager.PERMISSION_GRANTED;
    }

    /** The adapter when it can be used now, or null with `reason[0]` saying why not. */
    private static BluetoothAdapter usable(Context context, int[] reason) {
        if (!context.getPackageManager().hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE)) {
            reason[0] = NO_RADIO;
            return null;
        }
        BluetoothManager manager = context.getSystemService(BluetoothManager.class);
        BluetoothAdapter adapter = manager == null ? null : manager.getAdapter();
        if (adapter == null) {
            reason[0] = NO_RADIO;
            return null;
        }
        if (!allowed(context)) {
            reason[0] = NOT_ALLOWED;
            return null;
        }
        if (!adapter.isEnabled()) {
            reason[0] = OFF;
            return null;
        }
        return adapter;
    }

    private static boolean refusedLately(long at) {
        return at >= 0 && SystemClock.elapsedRealtime() - at < AGAIN_AFTER_MS;
    }

    /** Announce `tag`, or nothing when it is null. */
    static synchronized int announce(Context context, byte[] tag) {
        Context application = context.getApplicationContext();
        if (tag == null) {
            stopAnnouncing(application);
            advertiseRefusedAt = -1;
            return OK;
        }
        int[] reason = new int[1];
        BluetoothAdapter adapter = usable(application, reason);
        if (adapter == null) {
            // The system stopped whatever ran when Bluetooth went off or the permission was taken back: it is started again when it can be.
            advertising = null;
            announced = null;
            return reason[0];
        }
        if (advertising != null && Arrays.equals(announced, tag)) return OK;
        if (refusedLately(advertiseRefusedAt)) return FAILED;
        BluetoothLeAdvertiser advertiser = adapter.getBluetoothLeAdvertiser();
        if (advertiser == null) return NO_RADIO;
        try {
            if (advertising != null) advertiser.stopAdvertising(advertising);
            AdvertiseCallback callback = new AdvertiseCallback() {
                @Override public void onStartFailure(int errorCode) {
                    synchronized (NearbyRadio.class) {
                        if (advertising != this) return;
                        advertising = null;
                        announced = null;
                        advertiseRefusedAt = SystemClock.elapsedRealtime();
                    }
                    android.util.Log.w("Peersus", "Nearness could not be announced (" + errorCode + ")");
                }
            };
            advertiser.startAdvertising(
                    new AdvertiseSettings.Builder()
                            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_BALANCED)
                            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
                            .setConnectable(false)
                            .setTimeout(0)
                            .build(),
                    new AdvertiseData.Builder()
                            .setIncludeDeviceName(false)
                            .setIncludeTxPowerLevel(false)
                            .addServiceData(SERVICE, tag)
                            .build(),
                    callback);
            advertising = callback;
            announced = tag.clone();
            advertiseRefusedAt = -1;
            return OK;
        } catch (RuntimeException refused) {
            advertising = null;
            announced = null;
            advertiseRefusedAt = SystemClock.elapsedRealtime();
            return refused instanceof SecurityException ? NOT_ALLOWED : FAILED;
        }
    }

    private static void stopAnnouncing(Context application) {
        AdvertiseCallback callback = advertising;
        advertising = null;
        announced = null;
        if (callback == null) return;
        try {
            BluetoothManager manager = application.getSystemService(BluetoothManager.class);
            BluetoothAdapter adapter = manager == null ? null : manager.getAdapter();
            BluetoothLeAdvertiser advertiser = adapter == null ? null : adapter.getBluetoothLeAdvertiser();
            if (advertiser != null) advertiser.stopAdvertising(callback);
        } catch (RuntimeException gone) {
            // Bluetooth went off or the permission was taken back: the system has stopped it already.
        }
    }

    /**
     * Announce `offer`, an offer to pair, beside the tag, or nothing when it is null. It is longer than the older kind of
     * announcement carries, so it is sent as the extended kind; a phone whose radio has only the older kind refuses.
     */
    static synchronized int offer(Context context, byte[] offer) {
        Context application = context.getApplicationContext();
        if (offer == null) {
            stopOffering(application);
            offerRefusedAt = -1;
            return OK;
        }
        int[] reason = new int[1];
        BluetoothAdapter adapter = usable(application, reason);
        if (adapter == null) {
            offering = null;
            offered = null;
            return reason[0];
        }
        if (offering != null && Arrays.equals(offered, offer)) return OK;
        if (refusedLately(offerRefusedAt)) return FAILED;
        BluetoothLeAdvertiser advertiser = adapter.getBluetoothLeAdvertiser();
        if (advertiser == null) return NO_RADIO;
        try {
            if (!adapter.isLeExtendedAdvertisingSupported()
                    || offer.length + AROUND_DATA > adapter.getLeMaximumAdvertisingDataLength()) {
                offerRefusedAt = SystemClock.elapsedRealtime();
                Log.w("Peersus", "This phone's Bluetooth cannot announce an offer to pair: no extended announcements, or too short");
                return FAILED;
            }
            if (offering != null) advertiser.stopAdvertisingSet(offering);
            AdvertisingSetCallback callback = new AdvertisingSetCallback() {
                @Override public void onAdvertisingSetStarted(AdvertisingSet set, int txPower, int status) {
                    if (status == AdvertisingSetCallback.ADVERTISE_SUCCESS) return;
                    synchronized (NearbyRadio.class) {
                        if (offering != this) return;
                        offering = null;
                        offered = null;
                        offerRefusedAt = SystemClock.elapsedRealtime();
                    }
                    Log.w("Peersus", "An offer to pair could not be announced (" + status + ")");
                }
            };
            advertiser.startAdvertisingSet(
                    new AdvertisingSetParameters.Builder()
                            .setLegacyMode(false)
                            .setConnectable(false)
                            .setScannable(false)
                            .setInterval(AdvertisingSetParameters.INTERVAL_MEDIUM)
                            .setTxPowerLevel(AdvertisingSetParameters.TX_POWER_MEDIUM)
                            .setPrimaryPhy(BluetoothDevice.PHY_LE_1M)
                            .setSecondaryPhy(BluetoothDevice.PHY_LE_1M)
                            .build(),
                    new AdvertiseData.Builder()
                            .setIncludeDeviceName(false)
                            .setIncludeTxPowerLevel(false)
                            .addServiceData(OFFER, offer)
                            .build(),
                    null, null, null, callback);
            offering = callback;
            offered = offer.clone();
            offerRefusedAt = -1;
            return OK;
        } catch (RuntimeException refused) {
            offering = null;
            offered = null;
            offerRefusedAt = SystemClock.elapsedRealtime();
            return refused instanceof SecurityException ? NOT_ALLOWED : FAILED;
        }
    }

    private static void stopOffering(Context application) {
        AdvertisingSetCallback callback = offering;
        offering = null;
        offered = null;
        if (callback == null) return;
        try {
            BluetoothManager manager = application.getSystemService(BluetoothManager.class);
            BluetoothAdapter adapter = manager == null ? null : manager.getAdapter();
            BluetoothLeAdvertiser advertiser = adapter == null ? null : adapter.getBluetoothLeAdvertiser();
            if (advertiser != null) advertiser.stopAdvertisingSet(callback);
        } catch (RuntimeException gone) {
            // As for the tag: the system has stopped it already.
        }
    }

    /** Listen for announcements of the two services, or stop. */
    static synchronized int listen(Context context, boolean on) {
        Context application = context.getApplicationContext();
        if (!on) {
            stopListening(application);
            scanRefusedAt = -1;
            return OK;
        }
        int[] reason = new int[1];
        BluetoothAdapter adapter = usable(application, reason);
        if (adapter == null) {
            scanning = null;
            return reason[0];
        }
        if (scanning != null) {
            if (SystemClock.elapsedRealtime() - scanStartedAt < SCAN_ANEW_AFTER_MS) return OK;
            stopListening(application);
        }
        if (refusedLately(scanRefusedAt)) return FAILED;
        BluetoothLeScanner scanner = adapter.getBluetoothLeScanner();
        if (scanner == null) return NO_RADIO;
        try {
            ScanCallback callback = new ScanCallback() {
                @Override public void onScanResult(int callbackType, ScanResult result) {
                    ScanRecord record = result.getScanRecord();
                    if (record == null) return;
                    tell(record.getServiceData(SERVICE), MOST_TAG, result.getRssi(), 0);
                    tell(record.getServiceData(OFFER), MOST_OFFER, result.getRssi(), 1);
                }
                @Override public void onScanFailed(int errorCode) {
                    synchronized (NearbyRadio.class) {
                        if (scanning != this) return;
                        scanning = null;
                        scanRefusedAt = SystemClock.elapsedRealtime();
                    }
                    android.util.Log.w("Peersus", "Nearness could not be listened for (" + errorCode + ")");
                }
            };
            scanner.startScan(
                    // Any data of either service: an empty pattern is a prefix of every one.
                    Arrays.asList(
                            new ScanFilter.Builder().setServiceData(SERVICE, new byte[0]).build(),
                            new ScanFilter.Builder().setServiceData(OFFER, new byte[0]).build()),
                    new ScanSettings.Builder()
                            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
                            .setCallbackType(ScanSettings.CALLBACK_TYPE_ALL_MATCHES)
                            .setReportDelay(0)
                            // Announcements of the extended kind too: an offer to pair is one.
                            .setLegacy(false)
                            .build(),
                    callback);
            scanning = callback;
            scanStartedAt = SystemClock.elapsedRealtime();
            scanRefusedAt = -1;
            return OK;
        } catch (RuntimeException refused) {
            scanning = null;
            scanRefusedAt = SystemClock.elapsedRealtime();
            return refused instanceof SecurityException ? NOT_ALLOWED : FAILED;
        }
    }

    private static void stopListening(Context application) {
        ScanCallback callback = scanning;
        scanning = null;
        if (callback == null) return;
        try {
            BluetoothManager manager = application.getSystemService(BluetoothManager.class);
            BluetoothAdapter adapter = manager == null ? null : manager.getAdapter();
            BluetoothLeScanner scanner = adapter == null ? null : adapter.getBluetoothLeScanner();
            if (scanner != null) scanner.stopScan(callback);
        } catch (RuntimeException gone) {
            // As for announcing: there is nothing left to stop.
        }
    }

    private static void tell(byte[] data, int most, int strength, int kind) {
        if (data == null || data.length > most) return;
        if (Log.isLoggable(MEASURING, Log.DEBUG)) {
            Log.d(MEASURING, "heard kind=" + kind + " bytes=" + data.length + " dBm=" + strength);
        }
        heard(data, strength, kind);
    }

    /** One announcement that was heard: its data, its strength in dBm, and whose it is (0 the tag's, 1 an offer). Rust's. */
    private static native void heard(byte[] data, int strength, int kind);
}
