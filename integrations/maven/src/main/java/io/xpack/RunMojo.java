package io.xpack;

import io.xpack.internal.Json;
import io.xpack.internal.Processes;
import io.xpack.internal.Target;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.Execute;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;

/**
 * Installs into a throwaway root and starts the application.
 *
 * <p>The developer loop. It runs the installed launcher rather than asking
 * the command line to start the application, so what a developer sees is the
 * same path a user's machine takes — including the health check and a
 * rollback if the version fails to start.
 *
 * <p>Running this goal on its own builds the project first. Asking for
 * {@code mvn xpack:run} and being told there is no jar would be a pointless
 * step to make someone repeat, so the goal forks the lifecycle up to
 * {@code package} and everything bound there — including the other xPack
 * goals — has already run by the time this one starts.
 */
@Mojo(name = "run", requiresProject = true, threadSafe = true)
@Execute(phase = LifecyclePhase.PACKAGE)
public class RunMojo extends AbstractXPackMojo {

    /** The throwaway installation root. */
    @Parameter(property = "xpack.run.root", defaultValue = "${project.build.directory}/xpack/run")
    private java.io.File runRoot;

    /** Clears the root first, so each run starts from nothing. */
    @Parameter(property = "xpack.run.clean", defaultValue = "false")
    private boolean clean;

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }

        Target host = Target.host();
        Path pkg = packageFor(host);
        Path root = runRoot.toPath();
        if (clean) {
            deleteRecursively(root);
        }
        try {
            Files.createDirectories(root);
        } catch (IOException e) {
            throw new MojoExecutionException("could not create " + root, e);
        }

        List<String> install = new java.util.ArrayList<>(List.of(
                pkg.toString(), "--root", root.toString()));
        if (publicKey != null) {
            install.add("--trust");
            install.add(publicKey.toString());
        } else {
            // A fresh root trusts nothing, and this installation exists only
            // for this build, so accepting the key it was signed with is the
            // whole of the trust decision being made.
            install.add("--trust-on-first-use");
        }
        cli().run("install", install);

        Path launcher = root.resolve(applicationId())
                .resolve(host.executableName("xpack-launcher"));
        if (!Files.isRegularFile(launcher)) {
            throw new MojoExecutionException("no launcher at " + launcher);
        }

        getLog().info("xpack: starting " + launcher);
        Processes.run(List.of(launcher.toString()), getLog(), timeoutMinutes)
                .logDiagnostics(getLog())
                .failOnError("the application");
    }

    private Path packageFor(Target host) throws MojoExecutionException {
        Path dist = distDirectory.toPath();
        if (!Files.isDirectory(dist)) {
            throw new MojoExecutionException("no packages in " + dist + "; run xpack:pack first");
        }
        List<Path> candidates;
        try (Stream<Path> files = Files.list(dist)) {
            candidates = files.filter(p -> p.getFileName().toString().endsWith(".xpkg"))
                    .sorted().toList();
        } catch (IOException e) {
            throw new MojoExecutionException("could not list " + dist, e);
        }
        for (Path candidate : candidates) {
            Map<String, Object> described = cli().json("inspect", List.of(candidate.toString()));
            if (host.id().equals(Json.string(described, "platform"))) {
                return candidate;
            }
        }
        throw new MojoExecutionException(
                "nothing built for " + host.id() + ", so there is nothing to run here");
    }
}
