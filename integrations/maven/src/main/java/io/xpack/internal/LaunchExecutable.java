package io.xpack.internal;

import java.nio.file.Path;

/**
 * What the launcher is told to start.
 *
 * <p>A manifest distinguishes the two cases by shape alone: a value with a
 * path separator names a file inside the payload, and a bare name is looked
 * up on the machine's {@code PATH} when the application starts. That single
 * character is the difference between an installation that carries its own
 * runtime and one that borrows the machine's, so the choice is made here,
 * once, rather than in the goal that happens to need it.
 */
public final class LaunchExecutable {

    private LaunchExecutable() {
    }

    /**
     * Resolves the launch executable for one target.
     *
     * @param bundled whether a runtime ships inside the payload
     * @param command the interpreter to look up on PATH, when it does not
     * @param target the platform being built
     * @param payload the assembled payload, searched when a runtime is bundled
     * @param runtimeDirectory the runtime's name inside the payload
     * @throws IllegalStateException when the configuration cannot produce one
     */
    public static String resolve(
            boolean bundled,
            String command,
            Target target,
            Path payload,
            String runtimeDirectory) {
        if (bundled) {
            String found = target.findBundledJava(payload, runtimeDirectory);
            if (found == null) {
                throw new IllegalStateException(
                        "no bundled interpreter under " + payload.resolve(runtimeDirectory)
                                + ". Run xpack:runtime before xpack:manifest, or set "
                                + "<runtime><bundled>false</bundled> to start the machine's own.");
            }
            return found;
        }

        if (command == null || command.isBlank()) {
            throw new IllegalStateException(
                    "no runtime is bundled, so <runtime><command> must name the interpreter "
                            + "to start");
        }
        // A separator would be read as a payload-relative path. The file is
        // not in the payload, so the package would be refused for naming a
        // launch executable it does not contain — a confusing way to discover
        // a typo, and one that only shows up at packaging time.
        if (command.indexOf('/') >= 0 || command.indexOf('\\') >= 0) {
            throw new IllegalStateException(
                    "<runtime><command> is looked up on the PATH, so it must be a bare name "
                            + "rather than a path: " + command);
        }
        return target.executableName(command);
    }
}
