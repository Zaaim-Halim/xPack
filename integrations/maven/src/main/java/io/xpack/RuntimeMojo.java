package io.xpack;

import io.xpack.internal.Jdk;
import io.xpack.internal.Layout;
import io.xpack.internal.Target;
import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.List;
import java.util.stream.Collectors;
import org.apache.maven.artifact.Artifact;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.ResolutionScope;

/**
 * Links the Java runtime that ships inside the package.
 *
 * <p>A runtime is most of a package, and the only reason an installation does
 * not care what is already on the user's machine.
 */
@Mojo(name = "runtime",
        defaultPhase = LifecyclePhase.PREPARE_PACKAGE,
        requiresDependencyResolution = ResolutionScope.RUNTIME,
        threadSafe = true)
public class RuntimeMojo extends AbstractXPackMojo {

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }

        Layout layout = layout();
        for (Target target : targets()) {
            Jdk jdk = jdkFor(target);
            List<String> modules = modules(jdk);
            Path output = layout.runtime(target);

            getLog().info("xpack: linking a runtime for " + target.id() + " from " + jdk.home());
            getLog().info("xpack: modules " + String.join(",", modules));

            // jlink refuses to write into a directory that exists, so a
            // rebuild has to clear the previous image first.
            deleteRecursively(output);
            try {
                Files.createDirectories(output.getParent());
            } catch (IOException e) {
                throw new MojoExecutionException("could not create " + output.getParent(), e);
            }

            jdk.link(modules, output, runtime, getLog(), timeoutMinutes);

            String java = target.findBundledJava(layout.payload(target), runtimeDirectory);
            if (java == null) {
                throw new MojoExecutionException(
                        "jlink produced no interpreter for " + target.id() + " in " + output);
            }
            getLog().info("xpack: runtime ready, interpreter at " + java);
        }
    }

    /**
     * The JDK a target's runtime is linked from.
     *
     * <p>A foreign target needs that platform's own modules: the image is made
     * of them, and the local JDK only supplies the linker.
     */
    private Jdk jdkFor(Target target) throws MojoExecutionException {
        String configured = targetJdks.get(target.id());
        if (configured != null && !configured.isBlank()) {
            Jdk jdk = Jdk.at(Paths.get(configured));
            if (!jdk.hasModules()) {
                throw new MojoExecutionException(
                        "no jmods directory in the JDK configured for " + target.id() + ": "
                                + jdk.home());
            }
            return jdk;
        }

        if (!target.equals(Target.host())) {
            throw new MojoExecutionException(
                    "no JDK configured for " + target.id() + ", and the JDK running this build is "
                            + Target.host().id() + ". A runtime is made of the target platform's "
                            + "own modules, so cross-building needs that platform's JDK:\n"
                            + "  <targetJdks><" + target.id() + ">/path/to/jdk</" + target.id()
                            + "></targetJdks>\n"
                            + "or build this target on a " + target.os().id() + " machine.");
        }

        Jdk local = Jdk.current();
        if (!local.hasModules()) {
            throw new MojoExecutionException(
                    "the JDK running this build has no jmods directory (" + local.home()
                            + "). A JRE cannot link a runtime; build with a JDK.");
        }
        return local;
    }

    /** The configured module list, or the one jdeps computes. */
    private List<String> modules(Jdk jdk) throws MojoExecutionException {
        if (runtime.getModules() != null && !runtime.getModules().isEmpty()) {
            return runtime.getModules();
        }
        getLog().info("xpack: no <modules> configured, asking jdeps");
        return jdk.moduleDependencies(projectArtifact(), runtimeClasspath(), getLog(),
                timeoutMinutes);
    }

    private String runtimeClasspath() {
        return project.getArtifacts().stream()
                .filter(a -> a.getFile() != null)
                .map(Artifact::getFile)
                .map(File::toString)
                .collect(Collectors.joining(File.pathSeparator));
    }
}
