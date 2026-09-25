package io.xpack;

import io.xpack.internal.Json;
import io.xpack.internal.ReleaseArchive;
import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.Component;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;
import org.eclipse.aether.RepositorySystem;
import org.eclipse.aether.RepositorySystemSession;
import org.eclipse.aether.repository.RemoteRepository;

/**
 * Builds differential updates against releases already published.
 *
 * <p>A bundled runtime is most of a package and changes only when the JDK
 * does. A delta carries the files whose contents differ and reuses the rest
 * from the version already on the user's disk, so a release that changed one
 * jar ships that jar rather than the runtime again.
 *
 * <p>The earlier packages come from the repository this project deploys to,
 * which {@code xpack:pack} publishes them into. Nothing has to be kept by
 * hand, and a build that cannot reach them still succeeds: a delta is an
 * optimisation, and a client without one downloads the full package.
 */
@Mojo(name = "delta", defaultPhase = LifecyclePhase.VERIFY, threadSafe = true)
public class DeltaMojo extends AbstractXPackMojo {

    /**
     * How many earlier releases to build a delta from.
     *
     * <p>Three covers the users who update regularly while keeping a release
     * cheap to publish. Someone further behind than that downloads the full
     * package once, which is the correct outcome rather than a failure.
     *
     * <p>Zero turns repository resolution off entirely.
     */
    @Parameter(property = "xpack.delta.lastReleases", defaultValue = "3")
    private int lastReleases;

    /**
     * Whether a release may go out without the deltas it should have had.
     *
     * <p>It may not, by default. A delta is an optimisation for the client —
     * one that fails falls back to the full package, and nothing breaks — but
     * a *publisher* silently shipping none is not an optimisation being
     * declined. It is every user on the previous release downloading the
     * whole runtime again, on a release that looked like it succeeded.
     * Nobody notices that from the build log.
     *
     * <p>Having nothing to build from is different and never fails: a first
     * release has no earlier version, and that is simply the truth about it.
     */
    @Parameter(property = "xpack.delta.required", defaultValue = "true")
    private boolean required;

    /**
     * Specific earlier versions to build a delta from.
     *
     * <p>Resolved from the repository like {@link #lastReleases}, and used
     * instead of it when set — for a release that has to reach users of one
     * particular old version.
     */
    @Parameter
    private List<String> deltaFromVersions = new ArrayList<>();

    /**
     * Package files to build a delta from, named directly.
     *
     * <p>For packages that were never published to a repository. Added to
     * whatever the repository supplied.
     */
    @Parameter
    private List<File> deltaFromFiles = new ArrayList<>();

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

        List<Artefact> built = releaseArtefacts(".xpkg");
        if (built.isEmpty()) {
            throw new MojoExecutionException("no packages for " + releaseVersion() + " in "
                    + distDirectory + "; run xpack:pack first");
        }

        ReleaseArchive archive = new ReleaseArchive(
                repositorySystem, repositorySession, remoteRepositories, getLog());

        int total = 0;
        for (Artefact artefact : built) {
            total += buildFor(archive, artefact.platform(), artefact.file());
        }
        if (total == 0) {
            getLog().info("xpack: no earlier release to build a delta from, so every client "
                    + "will download the full package. Expected for a first release.");
        }
    }

    /** Builds every delta for one platform, and says how many there were. */
    private int buildFor(ReleaseArchive archive, String platform, Path target)
            throws MojoExecutionException {
        Map<String, Path> sources = new LinkedHashMap<>();

        List<String> versions;
        if (deltaFromVersions.isEmpty()) {
            ReleaseArchive.Releases released = archive.latestReleases(
                    project.getGroupId(), project.getArtifactId(), platform,
                    releaseVersion(), Math.max(lastReleases, 0));
            // Not knowing is not the same as there being none. A repository
            // that could not be reached hides however many releases exist,
            // and shipping as though there were none would quietly cost
            // every one of those users a full download.
            if (!released.known() && required) {
                throw new MojoExecutionException(
                        "could not ask the repository which releases exist for " + platform
                                + ", so no delta could be built. Fix the repository, or set "
                                + "-Dxpack.delta.required=false to publish without one.");
            }
            versions = released.versions();
        } else {
            versions = deltaFromVersions;
        }

        if (!versions.isEmpty()) {
            Map<String, Path> resolved = archive.resolveAll(
                    project.getGroupId(), project.getArtifactId(), platform, versions);
            List<String> missing = ReleaseArchive.missing(versions, resolved);
            if (!missing.isEmpty() && required) {
                throw new MojoExecutionException(
                        "no published package for " + String.join(", ", missing) + " (" + platform
                                + "), so users of those releases would download the whole "
                                + "package again. Publish them, narrow <lastReleases>, or set "
                                + "-Dxpack.delta.required=false.");
            }
            sources.putAll(resolved);
        }

        for (File file : deltaFromFiles) {
            Path path = file.toPath();
            if (!Files.isRegularFile(path)) {
                throw new MojoExecutionException("no package at " + path);
            }
            // Only the ones for this platform; the rest belong to another
            // iteration of this loop, and `xpack delta` would refuse them.
            if (platform.equals(platformOf(path))) {
                sources.put(path.toString(), path);
            }
        }

        int built = 0;
        for (Map.Entry<String, Path> source : sources.entrySet()) {
            Map<String, Object> result = cli().json("delta", List.of(
                    source.getValue().toString(), target.toString(),
                    "--out-dir", distDirectory.toString()));

            getLog().info("xpack: delta " + Json.string(result, "from")
                    + " -> " + Json.string(result, "to") + " (" + platform + ")  "
                    + Json.number(result, "changed") + " changed, "
                    + Json.number(result, "reused") + " reused, "
                    + human(Json.number(result, "size")));
            built++;
        }
        return built;
    }

    /** Asks the package itself rather than reading its filename. */
    private String platformOf(Path pkg) throws MojoExecutionException {
        return Json.platform(cli().inspect(pkg));
    }
}
