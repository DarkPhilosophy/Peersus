package dev.peersus.app;

import java.io.IOException;
import java.nio.file.FileSystemException;
import java.nio.file.Files;
import java.nio.file.Path;

/** Pure JVM filesystem regressions; no Activity or Keystore is used. */
public final class ExtensionAuthorizationTest {
    public static void main(String[] args) throws Exception {
        Path folder = Files.createTempDirectory("peersus-credential-test-");
        Path record = folder.resolve("record");
        Path pending = folder.resolve("record.new");
        Path backup = folder.resolve("record.bak");
        Path obstruction = backup.resolve("child");
        Path other = folder.resolve("other");
        try {
            Files.createFile(pending);
            ExtensionAuthorization.removeRecord(record);
            require(Files.notExists(pending), "an interrupted first save survived removal");

            Files.createFile(record);
            Files.createFile(pending);
            Files.createFile(backup);
            Files.createFile(other);
            ExtensionAuthorization.removeRecord(record);
            require(Files.notExists(record) && Files.notExists(pending) && Files.notExists(backup),
                    "an atomic record component survived removal");
            require(Files.isRegularFile(other), "removal touched another record");
            ExtensionAuthorization.removeRecord(record); // already absent is success

            Files.createFile(record);
            Files.createDirectory(backup);
            Files.createFile(obstruction);
            try {
                ExtensionAuthorization.removeRecord(record);
                throw new AssertionError("a failed delete was acknowledged");
            } catch (FileSystemException expected) {
                require(Files.isRegularFile(record), "base removed before the failing backup delete");
            }
        } finally {
            for (Path path : new Path[] { obstruction, backup, pending, record, other, folder }) {
                Files.deleteIfExists(path);
            }
        }
        System.out.println("Extension credential removal: 3 behavior scenarios passed");
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
