package dev.peersus.app;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.Proxy;
import java.net.URI;

/** Untrusted store bytes only; catalog signatures and package hashes remain the owner's job. */
final class StoreDownload {
    static final long MAX_BYTES = 128L * 1024 * 1024;
    private static final URI BASE = URI.create("https://darkphilosophy.github.io/Peersus/store/");
    private final URI base;

    static StoreDownload production() throws IOException { return new StoreDownload(BASE, true); }

    // A loopback HTTP base is used only by the dependency-free JVM transport tests.
    StoreDownload(URI base, boolean httpsOnly) throws IOException {
        String scheme = base.getScheme();
        boolean allowed = "https".equals(scheme) || (!httpsOnly && "http".equals(scheme)
                && ("127.0.0.1".equals(base.getHost()) || "::1".equals(base.getHost())));
        if (!allowed || base.getHost() == null || base.getRawUserInfo() != null
                || base.getRawQuery() != null || base.getRawFragment() != null
                || base.getPath() == null || !base.getPath().endsWith("/")) {
            throw new IOException("The store address is not allowed");
        }
        this.base = base;
    }

    URI address(String name) throws IOException {
        if (name == null || name.length() > 128 || !name.matches("[A-Za-z0-9][A-Za-z0-9._-]*")) {
            throw new IOException("The file name is not a plain name");
        }
        return base.resolve(name);
    }

    void copy(String name, long limit, OutputStream output) throws IOException {
        if (limit < 0 || limit > MAX_BYTES) throw new IOException("The download bound is not allowed");
        HttpURLConnection connection = (HttpURLConnection) address(name).toURL().openConnection(Proxy.NO_PROXY);
        try {
            connection.setInstanceFollowRedirects(false);
            connection.setUseCaches(false);
            connection.setAllowUserInteraction(false);
            connection.setConnectTimeout(10000);
            connection.setReadTimeout(10000);
            connection.setRequestMethod("GET");
            int status = connection.getResponseCode();
            if (status < 200 || status >= 300) throw new IOException("The store answered with an error");
            // Never trust Content-Length, including a chunked response with no length.
            try (InputStream input = connection.getInputStream()) {
                byte[] buffer = new byte[8192];
                long total = 0;
                int count;
                while ((count = input.read(buffer)) != -1) {
                    if (count > limit - total) throw new IOException("What the store sent is larger than allowed");
                    output.write(buffer, 0, count);
                    total += count;
                }
            }
        } finally {
            connection.disconnect();
        }
    }
}
