package dev.peersus.app;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.Proxy;
import java.net.URI;
import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;

/** Untrusted store bytes only; catalog signatures and package hashes remain the owner's job. */
final class StoreDownload {
    static final long MAX_BYTES = 128L * 1024 * 1024;
    private final URI base;
    private String revision;
    private String token;
    private URI assets;

    static StoreDownload production() throws IOException {
        return githubPublic("DarkPhilosophy/Peersus", "extension", "extension");
    }

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

    static StoreDownload githubPrivate(String repository, String directory, String revision, String token) throws IOException {
        if (token == null) throw new IOException("The private catalog configuration is not valid");
        return github(repository, directory, revision, token);
    }

    static StoreDownload githubPublic(String repository, String directory, String revision) throws IOException {
        return github(repository, directory, revision, null);
    }

    private static StoreDownload github(String repository, String directory, String revision, String token) throws IOException {
        String component = "[A-Za-z0-9._-]+";
        if (repository == null || repository.length() > 256 || repository.split("/", -1).length != 2
                || directory == null || directory.length() > 512 || revision == null || revision.length() > 256
                || (token != null && (token.isEmpty() || token.length() > 512 || !token.matches("[!-~]+")))) {
            throw new IOException("The private catalog configuration is not valid");
        }
        for (String value : new String[] {repository, directory, revision}) {
            for (String part : value.split("/", -1)) {
                if (!part.matches(component) || part.equals(".") || part.equals("..")) {
                    throw new IOException("The private catalog configuration is not valid");
                }
            }
        }
        StoreDownload result = authenticated(URI.create("https://api.github.com/repos/" + repository + "/contents/" + directory + "/"),
                true, revision, token);
        result.assets = URI.create("https://api.github.com/repos/" + repository + "/releases/assets/");
        return result;
    }

    // Package-private transport seam also exercised by the loopback JVM tests.
    static StoreDownload authenticated(URI base, boolean httpsOnly, String revision, String token) throws IOException {
        if (!httpsOnly && !"127.0.0.1".equals(base.getHost()) && !"::1".equals(base.getHost())) {
            throw new IOException("The store address is not allowed");
        }
        StoreDownload result = new StoreDownload(base, httpsOnly);
        result.revision = revision;
        result.token = token;
        return result;
    }

    URI address(String name) throws IOException {
        if (name == null || name.length() > 128 || !name.matches("[A-Za-z0-9][A-Za-z0-9._-]*")) {
            throw new IOException("The file name is not a plain name");
        }
        URI address = base.resolve(name);
        return revision == null ? address : URI.create(address + "?ref=" + URLEncoder.encode(revision, StandardCharsets.UTF_8));
    }

    /** Repository-bound asset addressing; the CDN request must never carry the API token. */
    void copyAsset(long id, long limit, OutputStream output) throws IOException {
        if (assets == null || id <= 0) throw new IOException("The release asset is not valid");
        copyAssetFrom(assets.resolve(Long.toString(id)), limit, output, false);
    }

    static boolean allowedAssetRedirect(URI target) {
        return "https".equals(target.getScheme())
                && target.getRawUserInfo() == null && target.getRawFragment() == null
                && (target.getPort() == -1 || target.getPort() == 443)
                && ("release-assets.githubusercontent.com".equals(target.getHost())
                    || "objects.githubusercontent.com".equals(target.getHost()));
    }

    // The loopback exception exists only in the package-private JVM test seam.
    void copyAssetFrom(URI address, long limit, OutputStream output, boolean loopbackTest) throws IOException {
        if (limit < 0 || limit > MAX_BYTES) throw new IOException("The download bound is not allowed");
        boolean test = loopbackTest && loopback(base) && loopback(address);
        boolean api = assets != null && address.toString().startsWith(assets.toString())
                && address.toString().substring(assets.toString().length()).matches("[1-9][0-9]*");
        if (!api && !test) {
            throw new IOException("The release asset is not valid");
        }
        HttpURLConnection connection = open(address, true, "application/octet-stream");
        try {
            if (connection.getResponseCode() == 302) {
                URI target;
                try {
                    target = URI.create(connection.getHeaderField("Location"));
                } catch (IllegalArgumentException | NullPointerException invalid) {
                    throw new IOException("The asset redirect is not allowed");
                }
                if (!allowedAssetRedirect(target) && !(test && loopback(target))) {
                    throw new IOException("The asset redirect is not allowed");
                }
                connection.disconnect();
                connection = open(target, false, "application/octet-stream");
            }
            transfer(connection, limit, output);
        } finally {
            connection.disconnect();
        }
    }

    private static boolean loopback(URI address) {
        return "http".equals(address.getScheme()) && "127.0.0.1".equals(address.getHost())
                && address.getRawUserInfo() == null && address.getRawFragment() == null;
    }

    private HttpURLConnection open(URI address, boolean authenticated, String accept) throws IOException {
        HttpURLConnection connection = (HttpURLConnection) address.toURL().openConnection(Proxy.NO_PROXY);
        connection.setInstanceFollowRedirects(false);
        connection.setUseCaches(false);
        connection.setAllowUserInteraction(false);
        connection.setConnectTimeout(10000);
        connection.setReadTimeout(10000);
        connection.setRequestMethod("GET");
        connection.setRequestProperty("Accept", accept);
        connection.setRequestProperty("User-Agent", "extension-store");
        if (authenticated && token != null) {
            connection.setRequestProperty("Authorization", "Bearer " + token);
            connection.setRequestProperty("X-GitHub-Api-Version", "2022-11-28");
        }
        return connection;
    }

    private static void transfer(HttpURLConnection connection, long limit, OutputStream output) throws IOException {
        int status = connection.getResponseCode();
        if (status < 200 || status >= 300) throw new IOException("The store answered with an error");
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
    }

    static StoreDownload githubTest(URI apiBase, String revision, String token) throws IOException {
        if (!loopback(apiBase)) throw new IOException("The store address is not allowed");
        StoreDownload result = authenticated(apiBase.resolve("contents/extension/"), false, revision, token);
        result.assets = apiBase.resolve("releases/assets/");
        return result;
    }

    URI apiAddress(String name) throws IOException {
        if (assets == null || name == null) return null;
        URI root = assets.resolve("../../");
        if (name.equals("store-tree")) {
            return URI.create(root + "git/trees/" + URLEncoder.encode(revision, StandardCharsets.UTF_8) + "?recursive=1");
        }
        // The branch the application's languages are on, one directory per release; its files are fetched by blob.
        if (name.equals("languages-tree")) {
            return URI.create(root + "git/trees/languages?recursive=1");
        }
        if (name.startsWith("store-blob-")) {
            String sha = name.substring("store-blob-".length());
            if (!sha.matches("[0-9a-f]{40}")) throw new IOException("The source blob is not valid");
            return root.resolve("git/blobs/" + sha);
        }
        if (name.startsWith("store-release-")) {
            String value = name.substring("store-release-".length());
            int split = value.lastIndexOf("--v");
            if (split < 1) throw new IOException("The extension release is not valid");
            String plugin = value.substring(0, split);
            String version = value.substring(split + 3);
            if (plugin.length() > 128 || !plugin.matches("[A-Za-z0-9][A-Za-z0-9._-]*")
                    || !version.matches("[0-9]{1,20}\\.[0-9]{1,20}\\.[0-9]{1,20}")) {
                throw new IOException("The extension release is not valid");
            }
            return URI.create(root + "releases/tags/" + URLEncoder.encode("extension/" + plugin + "/v" + version, StandardCharsets.UTF_8));
        }
        // The application's own releases: the list of its tags (`v…`, never an extension's), then the release of one
        // tag. Which tag is newest, and what it means, the application decides.
        if (name.equals("app-tags")) {
            return URI.create(root + "git/matching-refs/tags/v");
        }
        if (name.startsWith("app-release-")) {
            String tag = name.substring("app-release-".length());
            if (!tag.matches("v[a-z0-9.-]{0,63}")) throw new IOException("The application release is not valid");
            return URI.create(root + "releases/tags/" + URLEncoder.encode(tag, StandardCharsets.UTF_8));
        }
        return null;
    }

    void copy(String name, long limit, OutputStream output) throws IOException {
        if (limit < 0 || limit > MAX_BYTES) throw new IOException("The download bound is not allowed");
        if (assets != null && name != null && name.startsWith("store-asset-")) {
            try {
                copyAsset(Long.parseLong(name.substring("store-asset-".length())), limit, output);
            } catch (NumberFormatException invalid) {
                throw new IOException("The release asset is not valid");
            }
            return;
        }
        URI api = apiAddress(name);
        HttpURLConnection connection = open(api == null ? address(name) : api, true,
                api == null || name.startsWith("store-blob-") ? "application/vnd.github.raw+json" : "application/vnd.github+json");
        try {
            boolean release = name != null && (name.startsWith("store-release-") || name.startsWith("app-release-"));
            if (api != null && release && connection.getResponseCode() == 404) {
                if (limit < 4) throw new IOException("What the store sent is larger than allowed");
                output.write("null".getBytes(StandardCharsets.US_ASCII));
                return;
            }
            transfer(connection, limit, output);
        } finally {
            connection.disconnect();
        }
    }
}
