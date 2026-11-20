package dev.peersus.app;
import android.content.ClipData;
import dev.peersus.app.IClipboardChange;
/** Private shell UserService; no writes, commands, paths, or network operations. */
interface IClipboardSource {
    boolean start(int user, IClipboardChange listener) = 0;
    ClipData read() = 1;
    void stop() = 2;
    void destroy() = 16777114;
}
