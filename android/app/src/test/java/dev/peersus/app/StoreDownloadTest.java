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
        privateSourceIsScopedAndAuthenticatedWithoutUrlCredentials();
        privateCredentialsAreNotForwardedThroughRedirects();
        releaseRedirectHasNoCredentialsAndRetainsByteBound();
        releaseRejectsSecondRedirectAndUnapprovedHosts();
        branchDiscoveryUsesRevisionBoundImmutableRequests();
        System.out.println("Store download: 10 behavior scenarios passed");
    }
    private static final class Reply implements AutoCloseable {
        final ServerSocket socket;
        final Thread worker;
        volatile String request;
        Reply(String response) throws IOException {
            socket = new ServerSocket(0, 1, InetAddress.getLoopbackAddress());
            worker = new Thread(() -> {
                try (var client = socket.accept()) {
                    client.setSoTimeout(2000);
                    var requestBytes = new ByteArrayOutputStream();
                    while (requestBytes.size() < 8192) {
                        int next = client.getInputStream().read();
                        if (next == -1) break;
                        requestBytes.write(next);
                        if (requestBytes.toString(StandardCharsets.US_ASCII).endsWith("\r\n\r\n")) break;
                    }
                    request = requestBytes.toString(StandardCharsets.US_ASCII);
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
        assertEquals("https://api.github.com/repos/DarkPhilosophy/Peersus/contents/extension/plugin.json?ref=extension",
                store.address("plugin.json").toString());
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

    private static void privateSourceIsScopedAndAuthenticatedWithoutUrlCredentials() throws Exception {
        StoreDownload remote = StoreDownload.githubPrivate("example/private", "store", "main", "fake-test-only");
        assertEquals("https://api.github.com/repos/example/private/contents/store/catalog.json?ref=main",
                remote.address("catalog.json").toString());
        for (String bad : new String[] {"../private", "example/private/extra", "https://example/private"}) {
            assertThrows(IOException.class, () -> StoreDownload.githubPrivate(bad, "store", "main", "fake-test-only"));
        }
        assertThrows(IOException.class, () -> StoreDownload.githubPrivate("example/private", "../store", "main", "fake-test-only"));
        assertThrows(IOException.class, () -> StoreDownload.githubPrivate("example/private", "store", "main", "bad\nheader"));
        try (Reply reply = new Reply("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")) {
            ByteArrayOutputStream output = new ByteArrayOutputStream();
            StoreDownload.authenticated(reply.base(), false, "feature/catalog", "fake-test-only")
                    .copy("catalog.json", 5, output);
            assertArrayEquals("hello".getBytes(StandardCharsets.US_ASCII), output.toByteArray());
            String request = reply.request.toLowerCase(java.util.Locale.ROOT);
            assertTrue(request.startsWith("get /store/catalog.json?ref=feature%2fcatalog http/1.1\r\n"));
            assertTrue(request.contains("\r\nauthorization: bearer fake-test-only\r\n"));
            assertTrue(request.contains("\r\naccept: application/vnd.github.raw+json\r\n"));
        }
    }

    private static void privateCredentialsAreNotForwardedThroughRedirects() throws Exception {
        try (Reply reply = new Reply("HTTP/1.1 302 Found\r\nLocation: https://example.invalid/outside\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")) {
            IOException error = assertThrows(IOException.class, () ->
                StoreDownload.authenticated(reply.base(), false, "main", "fake-test-only")
                    .copy("catalog.json", 100, new ByteArrayOutputStream()));
            assertEquals("The store answered with an error", error.getMessage());
        }
    }

    private static void releaseRedirectHasNoCredentialsAndRetainsByteBound() throws Exception {
        for (int limit : new int[] {4, 5}) {
            try (Reply cdn = new Reply("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
                 Reply api = new Reply("HTTP/1.1 302 Found\r\nLocation: " + cdn.base().resolve("blob") + "\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")) {
                StoreDownload store = StoreDownload.authenticated(api.base(), false, "extension", "fake-test-only");
                ByteArrayOutputStream output = new ByteArrayOutputStream();
                if (limit == 5) {
                    store.copyAssetFrom(api.base().resolve("42"), limit, output, true);
                    assertArrayEquals("hello".getBytes(StandardCharsets.US_ASCII), output.toByteArray());
                } else {
                    assertThrows(IOException.class, () -> store.copyAssetFrom(api.base().resolve("42"), limit, output, true));
                    assertTrue(output.size() <= limit);
                }
                assertTrue(api.request.toLowerCase(java.util.Locale.ROOT).contains("\r\nauthorization: bearer fake-test-only\r\n"));
                assertTrue(api.request.toLowerCase(java.util.Locale.ROOT).contains("\r\naccept: application/octet-stream\r\n"));
                assertFalse(cdn.request.toLowerCase(java.util.Locale.ROOT).contains("\r\nauthorization:"));
            }
        }
    }

    private static void releaseRejectsSecondRedirectAndUnapprovedHosts() throws Exception {
        for (String allowed : new String[] {
                "https://release-assets.githubusercontent.com/blob?signature=test-only",
                "https://objects.githubusercontent.com/blob"}) {
            assertTrue(StoreDownload.allowedAssetRedirect(URI.create(allowed)));
        }
        for (String refused : new String[] {
                "http://objects.githubusercontent.com/blob",
                "https://objects.githubusercontent.com.example.org/blob",
                "https://example.org/blob",
                "https://user@objects.githubusercontent.com/blob",
                "https://objects.githubusercontent.com:444/blob",
                "https://objects.githubusercontent.com/blob#fragment"}) {
            assertFalse(StoreDownload.allowedAssetRedirect(URI.create(refused)));
        }
        try (Reply cdn = new Reply("HTTP/1.1 302 Found\r\nLocation: https://example.invalid/third\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
             Reply api = new Reply("HTTP/1.1 302 Found\r\nLocation: " + cdn.base().resolve("blob") + "\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")) {
            StoreDownload store = StoreDownload.authenticated(api.base(), false, "extension", "fake-test-only");
            assertThrows(IOException.class, () -> store.copyAssetFrom(api.base().resolve("42"), 100, new ByteArrayOutputStream(), true));
            assertFalse(cdn.request.toLowerCase(java.util.Locale.ROOT).contains("\r\nauthorization:"));
        }
        try (Reply api = new Reply("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")) {
            StoreDownload store = StoreDownload.authenticated(api.base(), false, "extension", "fake-test-only");
            ByteArrayOutputStream output = new ByteArrayOutputStream();
            store.copyAssetFrom(api.base().resolve("42"), 5, output, true);
            assertArrayEquals("hello".getBytes(StandardCharsets.US_ASCII), output.toByteArray());
        }
    }

    private static void branchDiscoveryUsesRevisionBoundImmutableRequests() throws Exception {
        String sha = "a".repeat(40);
        for (String[] scenario : new String[][] {
                {"store-tree", "/store/git/trees/extension?recursive=1", "application/vnd.github+json", "200 OK", "data"},
                {"store-blob-" + sha, "/store/git/blobs/" + sha, "application/vnd.github.raw+json", "200 OK", "data"},
                {"store-release-upload--v1.2.3", "/store/releases/tags/extension%2Fupload%2Fv1.2.3", "application/vnd.github+json", "404 Not Found", "null"},
                {"store-asset-42", "/store/releases/assets/42", "application/octet-stream", "200 OK", "data"}}) {
            try (Reply api = new Reply("HTTP/1.1 " + scenario[3] + "\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata")) {
                StoreDownload store = StoreDownload.githubTest(api.base(), "extension", "fake-test-only");
                ByteArrayOutputStream output = new ByteArrayOutputStream();
                store.copy(scenario[0], 4, output);
                assertEquals(scenario[4], output.toString(StandardCharsets.US_ASCII));
                String request = api.request.toLowerCase(java.util.Locale.ROOT);
                assertTrue(request.startsWith("get " + scenario[1].toLowerCase(java.util.Locale.ROOT) + " http/1.1"));
                assertTrue(request.contains("\r\naccept: " + scenario[2] + "\r\n"));
                assertTrue(request.contains("\r\nauthorization: bearer fake-test-only\r\n"));
            }
        }
    }
}
