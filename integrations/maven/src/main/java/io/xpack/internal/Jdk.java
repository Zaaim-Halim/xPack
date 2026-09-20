package io.xpack.internal;

import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugin.logging.Log;

/**
 * A JDK on disk, and the two tools this plugin runs out of one.
 *
 * <p>A runtime image is made of the <em>target</em> platform's modules, so
 * cross-building needs that platform's JDK rather than a flag. The linker
 * itself comes from whichever JDK is running the build.
 */
public final class Jdk {

    private final Path home;

    private Jdk(Path home) {
        this.home = home;
    }

    public static Jdk at(Path home) {
        return new Jdk(home);
    }

    /** The JDK running this build. */
    public static Jdk current() {
        return new Jdk(Paths.get(System.getProperty("java.home")));
    }

    public Path home() {
        return home;
    }

    /** Whether this is a JDK with linkable modules rather than a plain runtime. */
    public boolean hasModules() {
        return Files.isDirectory(home.resolve("jmods"));
    }

    public Path modules() {
        return home.resolve("jmods");
    }

    /** A tool inside this JDK, named for the platform the build runs on. */
    public Path tool(String name) {
        return home.resolve("bin").resolve(Target.host().executableName(name));
    }

    /** Links a runtime image into {@code output}, which must not already exist. */
    public void link(List<String> modules, Path output, io.xpack.config.RuntimeSpec spec,
            Log log, long timeoutMinutes) throws MojoExecutionException {
        List<String> command = new ArrayList<>();
        command.add(tool("jlink").toString());
        command.add("--module-path");
        command.add(modules().toString());
        command.add("--add-modules");
        command.add(String.join(",", modules));
        command.add("--output");
        command.add(output.toString());
        if (spec.isStripDebug()) {
            command.add("--strip-debug");
        }
        if (spec.isNoHeaderFiles()) {
            command.add("--no-header-files");
        }
        if (spec.isNoManPages()) {
            command.add("--no-man-pages");
        }
        if (spec.getCompress() != null && !spec.getCompress().isBlank()) {
            command.add("--compress");
            command.add(spec.getCompress());
        }
        command.addAll(spec.getExtraArguments());

        Processes.run(command, log, timeoutMinutes).failOnError("jlink");
    }

    /** Asks jdeps which modules an artefact and its classpath need. */
    public List<String> moduleDependencies(Path artifact, String classpath, Log log,
            long timeoutMinutes) throws MojoExecutionException {
        List<String> command = new ArrayList<>();
        command.add(tool("jdeps").toString());
        command.add("--print-module-deps");
        command.add("--ignore-missing-deps");
        if (classpath != null && !classpath.isEmpty()) {
            command.add("--class-path");
            command.add(classpath);
        }
        command.add(artifact.toString());

        Processes.Result result = Processes.run(command, log, timeoutMinutes);
        result.failOnError("jdeps");

        List<String> modules = new ArrayList<>(List.of(result.stdout().strip().split("[,\\s]+")));
        modules.removeIf(String::isBlank);
        if (modules.isEmpty()) {
            throw new MojoExecutionException(
                    "jdeps resolved no modules for " + artifact + ". Set them explicitly:\n"
                            + "  <runtime><modules><module>java.base</module></modules></runtime>");
        }
        return modules;
    }
}
