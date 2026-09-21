package io.xpack;

import io.xpack.internal.LaunchExecutable;
import io.xpack.internal.Layout;
import io.xpack.internal.ManifestWriter;
import io.xpack.internal.Target;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;

/**
 * Writes one {@code xpack.json} per target.
 *
 * <p>This is where every piece of Java-specific knowledge stops. Once this
 * file exists, nothing downstream knows what a jar is, what {@code -cp} means,
 * or that a JVM was ever involved.
 */
@Mojo(name = "manifest", defaultPhase = LifecyclePhase.PACKAGE, threadSafe = true)
public class ManifestMojo extends AbstractXPackMojo {

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }

        Layout layout = layout();
        for (Target target : targets()) {
            Path payload = layout.payload(target);
            String executable;
            try {
                executable = LaunchExecutable.resolve(
                        runtime.isBundled(), runtime.getCommand(), target, payload,
                        runtimeDirectory);
            } catch (IllegalStateException e) {
                throw new MojoExecutionException(e.getMessage(), e);
            }

            String manifest;
            try {
                manifest = new ManifestWriter()
                        .id(applicationId())
                        .name(applicationName())
                        .version(releaseVersion())
                        .description(description)
                        .publisher(publisherName())
                        .target(target)
                        .executable(executable)
                        .arguments(launchArguments())
                        .environment(environment)
                        .update(updateUrlFor(target), update)
                        .health(health)
                        .desktop(desktop)
                        .toJson();
            } catch (IllegalArgumentException | IllegalStateException e) {
                throw new MojoExecutionException(e.getMessage(), e);
            }

            Path file = layout.manifest(target);
            try {
                Files.createDirectories(file.getParent());
                Files.writeString(file, manifest, StandardCharsets.UTF_8);
            } catch (IOException e) {
                throw new MojoExecutionException("could not write " + file, e);
            }
            getLog().info("xpack: wrote " + file);
            getLog().debug(manifest);
        }
    }

    /**
     * How the interpreter is told to start the application.
     *
     * <p>With a main class, the whole of the application directory goes on the
     * class path through the wildcard the JVM expands itself, so adding a
     * dependency changes nothing here. Without one, the jar's own
     * {@code Main-Class} is used.
     */
    private List<String> launchArguments() throws MojoExecutionException {
        List<String> arguments = new ArrayList<>(jvmArgs);
        if (mainClass != null && !mainClass.isBlank()) {
            arguments.add("-cp");
            arguments.add("application/*");
            arguments.add(mainClass);
        } else {
            arguments.add("-jar");
            arguments.add("application/" + projectArtifact().getFileName());
        }
        arguments.addAll(appArgs);
        return arguments;
    }

    /**
     * The per-platform update URL.
     *
     * <p>The platform segment belongs on the end, because that is where the
     * channel document is served from. Appending it here rather than asking
     * for it means the manifest and the published layout cannot disagree.
     */
    private String updateUrlFor(Target target) {
        if (updateBaseUrl == null || updateBaseUrl.isBlank()) {
            return null;
        }
        String base = updateBaseUrl.strip();
        while (base.endsWith("/")) {
            base = base.substring(0, base.length() - 1);
        }
        return base + "/" + target.id();
    }

    private String publisherName() {
        if (publisher != null && !publisher.isBlank()) {
            return publisher;
        }
        if (project.getOrganization() != null) {
            return project.getOrganization().getName();
        }
        return null;
    }
}
