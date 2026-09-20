package io.xpack.internal;

import java.nio.file.Path;

/**
 * Where everything the plugin builds goes.
 *
 * <p>The goals are separate and a user may run any one of them on its own, so
 * none of them can hand a path to the next. They agree by computing the same
 * paths from the same rule, which is this class.
 */
public final class Layout {

    private final Path work;
    private final String runtimeDirectory;

    public Layout(Path work, String runtimeDirectory) {
        this.work = work;
        this.runtimeDirectory = runtimeDirectory;
    }

    /** Everything belonging to one target. */
    public Path target(Target target) {
        return work.resolve(target.id());
    }

    /** The tree that becomes the payload. */
    public Path payload(Target target) {
        return target(target).resolve("payload");
    }

    /** Where the linked runtime lives inside the payload. */
    public Path runtime(Target target) {
        return payload(target).resolve(runtimeDirectory);
    }

    /** Where the application's own jars live inside the payload. */
    public Path application(Target target) {
        return payload(target).resolve("application");
    }

    /** The generated project file the command line reads. */
    public Path manifest(Target target) {
        return target(target).resolve("xpack.json");
    }

    /** The runtime's name inside the payload, as a manifest would spell it. */
    public String runtimeDirectoryName() {
        return runtimeDirectory;
    }
}
