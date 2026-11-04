package dev.peersus.app;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.URI;
import java.nio.charset.StandardCharsets;

/** Pure JVM transport regressions, matching the existing javac/java test harness. */
public final class StoreDownloadTest {
    private interface Attempt { void run() throws Exception; }
    private static IOException assertThrows(Class<IOException> expected, Attempt attempt) throws Exception {
        try { attempt.run(); } catch (IOException error) { return error; }
        throw new AssertionError("Expected an I/O refusal");
    }
    private static void assertTrue(boolean value) { if (!value) throw new AssertionError(); }
    private static void assertFalse(boolean value) { assertTrue(!value); }
    private static void assertEquals(Object expected, Object actual) { assertTrue(expected.equals(actual)); }
    private static void assertArrayEquals(byte[] expected, byte[] actual) { assertTrue(java.util.Arrays.equals(expected, actual)); }
    public static void main(String[] args) throws Exception {
        productionRequiresHttpsAndPlainNames();
        returnsBodyAtExactBound();
        rejectsOversizedDeclaredAndChunkedBodies();
        refusesRedirectRatherThanFollowingIt();
        refusesErrorResponseWithoutReturningItsBody();
        System.out.println("Store download: 5 behavior scenarios passed");
    }
    private static final class Reply implements AutoCloseable {
        final ServerSocket socket;
        final Thread worker;
        Reply(String response) throws IOException {
            socket = new ServerSocket(0, 1, InetAddress.getLoopbackAddress());
            worker = new Thread(() -> {
                try (var client = socket.accept()) {
                    client.setSoTimeout(2000);
                    client.getInputStream().read(new byte[4096]);
                    client.getOutputStream().write(response.getBytes(StandardCharsets.US_ASCII));
                } catch (IOException ignored) { }
            });
            worker.setDaemon(true);
            worker.start();
        }
        URI base() { return URI.create("http://127.0.0.1:" + socket.getLocalPort() + "/store/"); }
        @Override public void close() throws Exception { socket.close(); worker.join(2500); assertFalse(worker.isAlive()); }
    }

    private static void productionRequiresHttpsAndPlainNames() throws Exception {
        assertThrows(IOException.class, () -> new StoreDownload(URI.create("http://example.org/store/"), true));
        assertThrows(IOException.class, () -> new StoreDownload(URI.create("https://user@example.org/store/"), true));
        assertThrows(IOException.class, () -> new StoreDownload(URI.create("https://example.org/store/?x"), true));
        assertThrows(IOException.class, () -> new StoreDownload(URI.create("https://example.org/store"), true));
        StoreDownload store = StoreDownload.production();
        for (String bad : new String[]{"", ".hidden", "../x", "a/b", "a\\b", "a?b", "a#b", "%2e%2e", "é", "x".repeat(129)}) {
            assertThrows(IOException.class, () -> store.address(bad));
        }
        assertTrue(store.address("catalog.json").toString().endsWith("/store/catalog.json"));
    }

    private static void returnsBodyAtExactBound() throws Exception {
        try (Reply reply = new Reply("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")) {
            ByteArrayOutputStream output = new ByteArrayOutputStream();
            new StoreDownload(reply.base(), false).copy("catalog.json", 5, output);
            assertArrayEquals("hello".getBytes(StandardCharsets.US_ASCII), output.toByteArray());
        }
    }

    private static void rejectsOversizedDeclaredAndChunkedBodies() throws Exception {
        for (String response : new String[]{
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n"}) {
            try (Reply reply = new Reply(response)) {
                ByteArrayOutputStream output = new ByteArrayOutputStream();
                assertThrows(IOException.class, () -> new StoreDownload(reply.base(), false).copy("a.bin", 4, output));
                assertTrue(output.size() <= 4);
            }
        }
    }

    private static void refusesRedirectRatherThanFollowingIt() throws Exception {
        try (Reply reply = new Reply("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/outside\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")) {
            IOException error = assertThrows(IOException.class, () -> new StoreDownload(reply.base(), false).copy("catalog.json", 100, new ByteArrayOutputStream()));
            assertEquals("The store answered with an error", error.getMessage());
        }
    }

    private static void refusesErrorResponseWithoutReturningItsBody() throws Exception {
        try (Reply reply = new Reply("HTTP/1.1 404 Not Found\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")) {
            ByteArrayOutputStream output = new ByteArrayOutputStream();
            assertThrows(IOException.class, () -> new StoreDownload(reply.base(), false).copy("catalog.json", 100, output));
            assertEquals(0, output.size());
        }
    }
}
