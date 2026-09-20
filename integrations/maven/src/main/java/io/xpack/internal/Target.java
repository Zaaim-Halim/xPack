package io.xpack.internal;

import java.io.File;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Locale;

/**
 * One operating system and architecture a package is built for.
 *
 * <p>Written as {@code <os>-<arch>}, the same spelling the command line takes
 * for {@code --platform} and uses in an artefact's name.
 */
public final class Target {

    /** Operating systems xPack installs onto. */
    public enum Os {
        WINDOWS("windows"),
        MACOS("macos"),
        LINUX("linux");

        private final String id;

        Os(String id) {
            this.id = id;
        }

        public String id() {
            return id;
        }
    }

    /** Architectures xPack installs onto. */
    public enum Arch {
        X64("x64"),
        ARM64("arm64");

        private final String id;

        Arch(String id) {
            this.id = id;
        }

        public String id() {
            return id;
        }
    }

    private final Os os;
    private final Arch arch;

    public Target(Os os, Arch arch) {
        this.os = os;
        this.arch = arch;
    }

    /**
     * Parses {@code <os>-<arch>}, accepting the spellings a build is likely to
     * already have in a property or a CI matrix.
     */
    public static Target parse(String text) {
        String value = text.trim().toLowerCase(Locale.ROOT);
        int split = value.lastIndexOf('-');
        if (split <= 0 || split == value.length() - 1) {
            throw new IllegalArgumentException(
                    "target \"" + text + "\" is not <os>-<arch>, for example linux-x64");
        }
        return new Target(parseOs(value.substring(0, split)), parseArch(value.substring(split + 1)));
    }

    private static Os parseOs(String value) {
        return switch (value) {
            case "windows", "win", "win32" -> Os.WINDOWS;
            case "macos", "osx", "darwin", "mac" -> Os.MACOS;
            case "linux" -> Os.LINUX;
            default -> throw new IllegalArgumentException(
                    "unknown operating system \"" + value + "\"; expected windows, macos or linux");
        };
    }

    private static Arch parseArch(String value) {
        return switch (value) {
            case "x64", "x86_64", "amd64" -> Arch.X64;
            case "arm64", "aarch64" -> Arch.ARM64;
            default -> throw new IllegalArgumentException(
                    "unknown architecture \"" + value + "\"; expected x64 or arm64");
        };
    }

    /** The platform this build is running on. */
    public static Target host() {
        String osName = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        Os os;
        if (osName.contains("win")) {
            os = Os.WINDOWS;
        } else if (osName.contains("mac") || osName.contains("darwin")) {
            os = Os.MACOS;
        } else {
            os = Os.LINUX;
        }
        return new Target(os, parseArch(System.getProperty("os.arch", "x86_64")));
    }

    public Os os() {
        return os;
    }

    public Arch arch() {
        return arch;
    }

    /** The {@code <os>-<arch>} spelling the command line expects. */
    public String id() {
        return os.id() + "-" + arch.id();
    }

    /** The name an executable has on this platform. */
    public String executableName(String base) {
        return os == Os.WINDOWS ? base + ".exe" : base;
    }

    /**
     * Finds the bundled Java interpreter inside a payload, as a payload-relative
     * path with forward slashes.
     *
     * <p>Probed rather than assumed. A {@code jlink} image puts the interpreter
     * at {@code runtime/bin/java} on every platform including macOS, while a
     * copied macOS JDK is a bundle and puts it at
     * {@code runtime/Contents/Home/bin/java}. Choosing from the operating
     * system alone gets one of those two wrong.
     *
     * @param payload the payload directory, which must already contain the runtime
     * @param runtimeDirectory the runtime's name inside the payload
     * @return the payload-relative path, or null when neither layout is present
     */
    public String findBundledJava(Path payload, String runtimeDirectory) {
        String java = executableName("java");
        String[] candidates = {
            runtimeDirectory + "/bin/" + java,
            runtimeDirectory + "/Contents/Home/bin/" + java,
        };
        for (String candidate : candidates) {
            if (Files.isRegularFile(payload.resolve(candidate.replace('/', File.separatorChar)))) {
                return candidate;
            }
        }
        return null;
    }

    @Override
    public String toString() {
        return id();
    }

    @Override
    public boolean equals(Object other) {
        return other instanceof Target t && t.os == os && t.arch == arch;
    }

    @Override
    public int hashCode() {
        return os.hashCode() * 31 + arch.hashCode();
    }
}
