package io.xpack.internal;

import java.util.ArrayList;
import java.util.List;

/**
 * The arguments the bundled or system {@code java} is started with.
 *
 * <p>The application's jars are named relative to the package. The launcher
 * starts the application in its installed version directory, where
 * {@code application/*} finds them. An application that keeps the directory
 * it was run from starts somewhere else, so there the path is written with
 * {@code {versionDir}}, which the launcher replaces with the version directory.
 * Only then: the placeholder makes a package format 2, which installations of
 * xPack 0.1.0 cannot update to, and an application that does not need it
 * should not pay that.
 */
public final class LaunchArguments {

    /** Replaced by the launcher with the installed version directory. */
    public static final String VERSION_DIR = "{versionDir}";

    private LaunchArguments() {}

    /**
     * Builds the arguments.
     *
     * @param jvmArgs passed first, as written
     * @param mainClass the class to start, or blank to use the jar's own
     * @param jarName the project's jar, used when there is no main class
     * @param appArgs passed last, as written
     * @param keepWorkingDirectory whether the application starts where it was run from
     */
    public static List<String> of(
            List<String> jvmArgs,
            String mainClass,
            String jarName,
            List<String> appArgs,
            boolean keepWorkingDirectory) {
        String application = (keepWorkingDirectory ? VERSION_DIR + "/" : "") + "application/";
        List<String> arguments = new ArrayList<>(jvmArgs);
        if (mainClass != null && !mainClass.isBlank()) {
            arguments.add("-cp");
            arguments.add(application + "*");
            arguments.add(mainClass);
        } else {
            arguments.add("-jar");
            arguments.add(application + jarName);
        }
        arguments.addAll(appArgs);
        return arguments;
    }
}
