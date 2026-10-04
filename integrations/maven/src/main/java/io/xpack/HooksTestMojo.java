package io.xpack;

import io.xpack.config.InstallerUiSpec;
import io.xpack.internal.Json;
import io.xpack.internal.ReleaseArchive;
import java.io.File;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.Component;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;
import org.eclipse.aether.RepositorySystem;
import org.eclipse.aether.RepositorySystemSession;
import org.eclipse.aether.repository.RemoteRepository;

/**
 * Runs the release's hooks with {@code xpack hooks test}, as users' machines
 * will run them, and leaves its report beside each package.
 *
 * <p>Bound to {@code integration-test}: after {@code xpack:pack}, before
 * {@code xpack:delta} and {@code xpack:index}, which ship a package with
 * hooks only once a passing report for it is there, so an ordinary
 * {@code mvn verify} passes the gate when the hooks pass. {@code xpack:installer}
 * reads the same reports.
 *
 * <p>One report for an installation for one user, always: only those update
 * themselves. One for an installation for everyone too, when
 * {@code <installerUi><allUsers>} offers it or always does. The update
 * scenario runs over the latest release in the repository, or the one named.
 * Nothing is checked here: every rule is the command line's.
 */
@Mojo(name = "hooks-test", defaultPhase = LifecyclePhase.INTEGRATION_TEST, threadSafe = true)
public class HooksTestMojo extends AbstractXPackMojo {

    /**
     * Do what the hooks do: run their programs, write their files. For a
     * disposable machine of the target platform, a CI runner among them,
     * never a developer's own. A mandatory release ships only after one.
     */
    @Parameter(property = "xpack.hooks.real", defaultValue = "false")
    private boolean real;

    /**
     * What programs answer when they are not run: a JSON object from a
     * program's name to {@code { "exitCode", "stdout", "stderr" }}.
     */
    @Parameter(property = "xpack.hooks.answers")
    private File answers;

    /**
     * The release the update scenario runs over, by version. Left out, the
     * latest release in the repository, if there is one.
     */
    @Parameter(property = "xpack.hooks.previousVersion")
    private String previousVersion;

    /** Earlier packages named directly; each is used for its own platform. */
    @Parameter
    private List<File> previousFiles = new ArrayList<>();

    /** The installer's settings, which say whether it installs for everyone. */
    @Parameter
    private InstallerUiSpec installerUi;

    @Parameter(defaultValue = "${repositorySystemSession}", readonly = true, required = true)
    private RepositorySystemSession repositorySession;

    @Parameter(defaultValue = "${project.remoteProjectRepositories}", readonly = true)
    private List<RemoteRepository> remoteRepositories = new ArrayList<>();

    @Component
    private RepositorySystem repositorySystem;

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }
        if (hooks.isEmpty()) {
            getLog().info("xpack: no hooks to test");
            return;
        }
        List<Artefact> built = releaseArtefacts(".xpkg");
        if (built.isEmpty()) {
            throw new MojoExecutionException("no packages for " + releaseVersion() + " in "
                    + distDirectory + "; run xpack:pack first");
        }
        ReleaseArchive archive = new ReleaseArchive(
                repositorySystem, repositorySession, remoteRepositories, getLog());
        for (Artefact artefact : built) {
            Path previous = previousFor(archive, artefact.platform());
            if (previous == null) {
                getLog().info("xpack: no earlier release for " + artefact.platform()
                        + ", so its update scenario does not run");
            }
            for (boolean everyone : scopesFor(installerUi)) {
                cli().runReporting("hooks",
                        arguments(artefact.file(), previous, everyone, real, answers));
            }
        }
    }

    /**
     * The scopes to test, as "for everyone?": one user always, as only those
     * installations update themselves; everyone too where the installer
     * offers it or always installs so. Visible for tests.
     */
    static List<Boolean> scopesFor(InstallerUiSpec installerUi) {
        String allUsers = installerUi == null || installerUi.getAllUsers() == null
                ? "never" : installerUi.getAllUsers().trim().toLowerCase(Locale.ROOT);
        return switch (allUsers) {
            case "offer", "always" -> List.of(false, true);
            default -> List.of(false);
        };
    }

    /** The command line for one package and scope; visible for tests. */
    static List<String> arguments(Path pkg, Path previous, boolean everyone, boolean real,
            File answers) {
        List<String> arguments = new ArrayList<>();
        arguments.add("test");
        arguments.add(pkg.toString());
        if (previous != null) {
            arguments.add("--previous");
            arguments.add(previous.toString());
        }
        if (real) {
            arguments.add("--real");
        }
        if (answers != null) {
            arguments.add("--answers");
            arguments.add(answers.toString());
        }
        if (everyone) {
            arguments.add("--all-users");
        }
        return arguments;
    }

    /** The earlier package of `platform` the update scenario runs over. */
    private Path previousFor(ReleaseArchive archive, String platform)
            throws MojoExecutionException {
        for (File file : previousFiles) {
            Path path = file.toPath();
            if (!Files.isRegularFile(path)) {
                throw new MojoExecutionException("no package at " + path);
            }
            if (platform.equals(Json.platform(cli().inspect(path)))) {
                return path;
            }
        }
        List<String> versions;
        if (previousVersion != null && !previousVersion.isBlank()) {
            versions = List.of(previousVersion.trim());
        } else {
            ReleaseArchive.Releases released = archive.latestReleases(
                    project.getGroupId(), project.getArtifactId(), platform, releaseVersion(), 1);
            if (!released.known()) {
                // Not knowing is not the same as there being none: the gate
                // will ask for the update scenario if a release is published.
                getLog().warn("xpack: could not ask the repository which releases exist for "
                        + platform + ", so the update scenario does not run; name one with "
                        + "<previousVersion> or <previousFiles>");
            }
            versions = released.versions();
        }
        if (versions.isEmpty()) {
            return null;
        }
        Map<String, Path> resolved = archive.resolveAll(
                project.getGroupId(), project.getArtifactId(), platform, versions);
        if (resolved.isEmpty()) {
            if (previousVersion != null && !previousVersion.isBlank()) {
                throw new MojoExecutionException("no published package for " + previousVersion
                        + " (" + platform + ") to run the update scenario over");
            }
            return null;
        }
        return resolved.values().iterator().next();
    }
}
