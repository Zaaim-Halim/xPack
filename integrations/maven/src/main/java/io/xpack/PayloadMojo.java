package io.xpack;

import io.xpack.internal.Layout;
import io.xpack.internal.Target;
import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.List;
import org.apache.maven.artifact.Artifact;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;
import org.apache.maven.plugins.annotations.ResolutionScope;

/**
 * Assembles the tree that becomes the payload.
 *
 * <p>The application's jars go beside the runtime. Nothing else is added,
 * because everything in the payload is hashed, signed and shipped to every
 * user.
 */
@Mojo(name = "payload",
        defaultPhase = LifecyclePhase.PACKAGE,
        requiresDependencyResolution = ResolutionScope.RUNTIME,
        threadSafe = true)
public class PayloadMojo extends AbstractXPackMojo {

    /**
     * A directory copied into the payload root as it stands.
     *
     * <p>Where an icon named by {@code <desktop><icon>} comes from, along with
     * anything else the application needs beside its jars.
     */
    @Parameter(property = "xpack.payloadResources")
    private File payloadResources;

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }

        Path artifact = projectArtifact();
        List<Artifact> dependencies = runtimeDependencies();
        Layout layout = layout();

        for (Target target : targets()) {
            Path application = layout.application(target);
            deleteRecursively(application);
            createDirectories(application);

            copy(artifact, application.resolve(artifact.getFileName().toString()));
            for (Artifact dependency : dependencies) {
                Path file = dependency.getFile().toPath();
                copy(file, application.resolve(file.getFileName().toString()));
            }

            if (payloadResources != null) {
                if (!payloadResources.isDirectory()) {
                    throw new MojoExecutionException(
                            "payloadResources is not a directory: " + payloadResources);
                }
                copyTree(payloadResources.toPath(), layout.payload(target));
            }

            getLog().info("xpack: payload for " + target.id() + " has "
                    + (dependencies.size() + 1) + " application file(s)");
        }
    }

    private List<Artifact> runtimeDependencies() {
        List<Artifact> result = new ArrayList<>();
        for (Artifact artifact : project.getArtifacts()) {
            if (artifact.getFile() == null) {
                continue;
            }
            String scope = artifact.getScope();
            if (Artifact.SCOPE_COMPILE.equals(scope) || Artifact.SCOPE_RUNTIME.equals(scope)) {
                result.add(artifact);
            }
        }
        return result;
    }

    private void createDirectories(Path path) throws MojoExecutionException {
        try {
            Files.createDirectories(path);
        } catch (IOException e) {
            throw new MojoExecutionException("could not create " + path, e);
        }
    }

    private void copy(Path from, Path to) throws MojoExecutionException {
        try {
            Files.copy(from, to, StandardCopyOption.REPLACE_EXISTING);
        } catch (IOException e) {
            throw new MojoExecutionException("could not copy " + from + " to " + to, e);
        }
    }

    private void copyTree(Path from, Path to) throws MojoExecutionException {
        try (var paths = Files.walk(from)) {
            for (Path source : paths.toList()) {
                Path destination = to.resolve(from.relativize(source).toString());
                if (Files.isDirectory(source)) {
                    Files.createDirectories(destination);
                } else {
                    Files.createDirectories(destination.getParent());
                    Files.copy(source, destination, StandardCopyOption.REPLACE_EXISTING);
                }
            }
        } catch (IOException e) {
            throw new MojoExecutionException("could not copy " + from + " into " + to, e);
        }
    }
}
