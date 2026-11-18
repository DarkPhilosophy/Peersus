package dev.peersus.app;

/** Actual bounded descriptor I/O for instrumentation; no mocked application control. */
final class TestFrames {
    static void nonblocking(java.io.FileDescriptor fd) throws Exception {
        android.system.Os.fcntlInt(fd, android.system.OsConstants.F_SETFL,
            android.system.Os.fcntlInt(fd, android.system.OsConstants.F_GETFL, 0) | android.system.OsConstants.O_NONBLOCK);
    }
    static void readable(java.io.FileDescriptor fd) throws Exception {
        android.system.StructPollfd poll = new android.system.StructPollfd();
        poll.fd = fd; poll.events = (short)android.system.OsConstants.POLLIN;
        if (android.system.Os.poll(new android.system.StructPollfd[] {poll}, 5000) == 0) throw new java.io.IOException("descriptor readiness timed out");
    }
    static org.json.JSONObject readFrame(java.io.FileDescriptor fd) throws Exception { return readFrame(fd, 65536); }
    static org.json.JSONObject readFrame(java.io.FileDescriptor fd, int maximum) throws Exception {
        byte[] header = new byte[4]; transfer(fd, header, false);
        int length = java.nio.ByteBuffer.wrap(header).getInt();
        if (length <= 0 || length > maximum) throw new java.io.IOException("invalid frame size");
        byte[] bytes = new byte[length]; transfer(fd, bytes, false);
        return new org.json.JSONObject(new String(bytes, java.nio.charset.StandardCharsets.UTF_8));
    }
    static void transfer(java.io.FileDescriptor fd, byte[] bytes, boolean writing) throws Exception {
        long deadline = android.os.SystemClock.elapsedRealtime() + 5000;
        int offset = 0;
        while (offset < bytes.length) {
            long remaining = deadline - android.os.SystemClock.elapsedRealtime();
            if (remaining <= 0) throw new java.io.IOException("descriptor transfer timed out");
            android.system.StructPollfd poll = new android.system.StructPollfd();
            poll.fd = fd; poll.events = (short)(writing ? android.system.OsConstants.POLLOUT : android.system.OsConstants.POLLIN);
            if (android.system.Os.poll(new android.system.StructPollfd[] {poll}, (int)remaining) == 0) throw new java.io.IOException("descriptor transfer timed out");
            try {
                int amount = writing ? android.system.Os.write(fd, bytes, offset, bytes.length - offset)
                    : android.system.Os.read(fd, bytes, offset, bytes.length - offset);
                if (amount <= 0) throw new java.io.EOFException("descriptor closed");
                offset += amount;
            } catch (android.system.ErrnoException error) {
                if (error.errno != android.system.OsConstants.EAGAIN && error.errno != android.system.OsConstants.EINTR) throw error;
            }
        }
    }
    static org.json.JSONObject request(android.content.Context context, org.json.JSONObject request) throws Exception {
        try (android.net.LocalSocket socket = new android.net.LocalSocket()) {
            socket.connect(new android.net.LocalSocketAddress(new java.io.File(context.getFilesDir(), "control.sock").getAbsolutePath(),
                android.net.LocalSocketAddress.Namespace.FILESYSTEM));
            nonblocking(socket.getFileDescriptor());
            byte[] payload = request.toString().getBytes(java.nio.charset.StandardCharsets.UTF_8);
            transfer(socket.getFileDescriptor(), java.nio.ByteBuffer.allocate(4 + payload.length).putInt(payload.length).put(payload).array(), true);
            return readFrame(socket.getFileDescriptor(), 4 * 1024 * 1024);
        }
    }
}
