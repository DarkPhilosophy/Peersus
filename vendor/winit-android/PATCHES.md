Baseline: published `winit-android` 0.31.0-beta.4, [revision `d597ced770f3`](https://github.com/rust-windowing/winit/commit/d597ced770f3263f66ab901cc27bf7984e314465), with its upstream license. Both changes move from the former monolithic `vendor/winit` patch.

# Exit the loop when its NativeActivity is destroyed

`src/event_loop.rs`: `MainEvent::Destroy` calls `ActiveEventLoop::exit`. `NativeActivity.onDestroy` waits for `android_main` to return; logging the event and continuing to poll can block the Android main thread. This does not replace `destroy_surfaces`, which Winit calls on `MainEvent::TerminateWindow`.

Removal: drop when Winit forwards destruction in time for the application to return, or exits the loop itself. Verify activity teardown without an ANR.

# Allow a replacement NativeActivity to start its loop

`src/event_loop.rs`: remove the process-wide one-loop guard on Android. Each NativeActivity owns a distinct Android looper, and a replacement must start even if the original loop has not finished unwinding.

Removal: drop when Winit supports successive or overlapping NativeActivity lifetimes. Reopen the app after activity destruction and verify rendering and input.
