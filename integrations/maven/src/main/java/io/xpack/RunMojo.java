package io.xpack;

import io.xpack.internal.Json;
import io.xpack.internal.Processes;
import io.xpack.internal.Target;
import io.xpack.internal.XPackCli;
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
        XPackCli cli = cli();
        cli.run("install", install);

        // Asked for rather than worked out. An installation names its
        // executables after the application it serves, sanitising the name
        // and pinning the result the first time; reconstructing that here
        // would be a second implementation of a rule that has to stay in one
        // place, and would be wrong for any installation predating it.
        Map<String, Object> listing =
                cli.json("list", List.of(applicationId(), "--root", root.toString()));
        Object reported = listing.get("launcher");
        if (!(reported instanceof String path)) {
            throw new MojoExecutionException(
                    "the installation has no launcher, so there is nothing to run");
        }
        Path launcher = Path.of(path);

        getLog().info("xpack: starting " + launcher);
        int status = Processes.runInheritingIo(List.of(launcher.toString()), getLog());
        if (status != 0) {
            // The launcher's own exit code, which is how a rolled-back
            // version reports itself. Worth failing the build on: a developer
            // asking to run their application wants to know it did not start.
            throw new MojoExecutionException("the application exited with status " + status);
        }
    }

    /** This release's package for the platform the build is running on. */
    private Path packageFor(Target host) throws MojoExecutionException {
        for (Artefact artefact : releaseArtefacts(".xpkg")) {
            if (host.id().equals(artefact.platform())) {
                return artefact.file();
            }
        }
        throw new MojoExecutionException("nothing built for " + host.id()
                + " at version " + releaseVersion() + ", so there is nothing to run here");
    }
}
