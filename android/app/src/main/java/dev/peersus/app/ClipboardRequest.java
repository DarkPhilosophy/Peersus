package dev.peersus.app;

/** Deadline is fixed at operation admission, not renewed by queued platform requests. */
final class ClipboardRequest {
    final long deadline;
    private boolean active = true;

    ClipboardRequest(long deadline) { this.deadline = deadline; }

    synchronized boolean allowed(long now) { return active && now < deadline; }
    synchronized void cancel() { active = false; }
}
