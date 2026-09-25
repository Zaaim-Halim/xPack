package io.xpack;

import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;

/**
 * Writes the documents an update server publishes, one per platform.
 *
 * <p>The output directory is the <em>parent</em> of the per-platform
 * directories, because the platform segment is already on the end of the
 * update URL in every manifest. Get that wrong and the index is published one
 * directory away from where every client looks, with nothing failing at build
 * time.
 */
@Mojo(name = "index", defaultPhase = LifecyclePhase.DEPLOY, threadSafe = true)
public class IndexMojo extends AbstractXPackMojo {

    /** Where the per-platform directories are written. */
    @Parameter(property = "xpack.index.outDir",
            defaultValue = "${project.build.directory}/xpack/site")
    private File indexDirectory;

    /** Offer this release to a percentage of installations, 0 to 100. */
    @Parameter(property = "xpack.index.rollout")
    private Integer rollout;

    /** URL of the release notes, recorded in every index written. */
    @Parameter(property = "xpack.index.releaseNotes")
    private String releaseNotes;

    /**
     * Where the packages are downloaded from, when that is not beside the
     * index.
     *
     * <p>Left empty, each package is named by file name and uploaded next to
     * its platform's index. Set, each is named by its full address,
     * {@code <packageUrl>/<file name>}: the index can then live on a static
     * site while the packages are attached to a release. Must be
     * {@code https}; the command line refuses anything else.
     */
    @Parameter(property = "xpack.index.packageUrl")
    private String packageUrl;

    /**
     * Deltas to offer beside the full packages, named directly.
     *
     * <p>Left empty, every delta {@code xpack:delta} produced is offered.
     * Publishing a full package while quietly omitting the deltas built for
     * it is a saving thrown away, and the kind of omission nobody notices
     * because everything still works.
     */
    @Parameter
    private List<File> deltas = new ArrayList<>();

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }

        List<Path> packages =
                releaseArtefacts(".xpkg").stream().map(Artefact::file).toList();
        if (packages.isEmpty()) {
            throw new MojoExecutionException("no packages for " + releaseVersion() + " in "
                    + distDirectory + "; run xpack:pack first");
        }
        if (rollout != null && (rollout < 0 || rollout > 100)) {
            throw new MojoExecutionException("rollout must be between 0 and 100");
        }

        List<String> arguments = new ArrayList<>();
        packages.forEach(p -> arguments.add(p.toString()));
        arguments.add("--out-dir");
        arguments.add(indexDirectory.toString());
        if (publicKey != null) {
            // Verifying before indexing turns "a release nobody can install"
            // into a build failure rather than a support ticket.
            arguments.add("--key");
            arguments.add(publicKey.toString());
        } else {
            getLog().warn("xpack: no <publicKey>, so packages are indexed unverified");
        }
        if (rollout != null) {
            arguments.add("--rollout");
            arguments.add(String.valueOf(rollout));
        }
        if (releaseNotes != null && !releaseNotes.isBlank()) {
            arguments.add("--release-notes");
            arguments.add(releaseNotes);
        }
        if (packageUrl != null && !packageUrl.isBlank()) {
            arguments.add("--package-url");
            arguments.add(packageUrl);
        }
        for (Path delta : deltasToPublish()) {
            arguments.add("--delta");
            arguments.add(delta.toString());
        }

        for (Object entry : cli().jsonArray("index", arguments)) {
            if (entry instanceof Map<?, ?> map) {
                getLog().info("xpack: index " + map.get("platform") + " -> " + map.get("path"));
                getLog().info("xpack:   serve it at " + map.get("url"));
            }
        }
    }

    /** The deltas to offer: those named, or everything the build produced. */
    private List<Path> deltasToPublish() throws MojoExecutionException {
        if (!deltas.isEmpty()) {
            List<Path> named = new ArrayList<>();
            for (File delta : deltas) {
                Path path = delta.toPath();
                if (!Files.isRegularFile(path)) {
                    throw new MojoExecutionException("no delta at " + path);
                }
                named.add(path);
            }
            return named;
        }
        // Every delta this release produced. One left over from an earlier
        // release targets a version that is not being indexed, and the
        // command line refuses those rather than publishing something no
        // client could apply.
        return releaseArtefacts(".xpkgd").stream().map(Artefact::file).toList();
    }

}
