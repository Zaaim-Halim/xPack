package io.xpack;

import io.xpack.internal.Json;
import io.xpack.internal.Layout;
import io.xpack.internal.Target;
import io.xpack.internal.XPackCli;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.Component;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;
import org.apache.maven.project.MavenProjectHelper;

/** Builds one signed package per target. */
@Mojo(name = "pack", defaultPhase = LifecyclePhase.PACKAGE, threadSafe = true)
public class PackMojo extends AbstractXPackMojo {

    /**
     * Whether each package is attached to the project.
     *
     * <p>Attached, `mvn install` and `mvn deploy` publish the packages beside
     * the jar, one per platform, distinguished by a classifier. That makes
     * the repository the release archive — which is what lets a later release
     * build a differential update against this one without anybody keeping
     * artefacts by hand.
     *
     * <p>It also means a deploy uploads tens of megabytes per platform, so it
     * can be turned off for a build that has somewhere else to put them.
     */
    @Parameter(property = "xpack.attach", defaultValue = "true")
    private boolean attach;

    @Component
    private MavenProjectHelper projectHelper;

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }

        XPackCli cli = cli();
        Path key = signingKeyPath();
        if (!Files.isRegularFile(key)) {
            throw new MojoExecutionException("no signing key at " + key);
        }
        try {
            Files.createDirectories(distDirectory.toPath());
        } catch (IOException e) {
            throw new MojoExecutionException("could not create " + distDirectory, e);
        }

        Layout layout = layout();
        for (Target target : targets()) {
            Path payload = layout.payload(target);
            Path manifest = layout.manifest(target);
            if (!Files.isDirectory(payload)) {
                throw new MojoExecutionException(
                        "no payload at " + payload + ". Run xpack:payload first.");
            }
            if (!Files.isRegularFile(manifest)) {
                throw new MojoExecutionException(
                        "no manifest at " + manifest + ". Run xpack:manifest first.");
            }
            // Packaging refuses a signing key found in a payload, but failing
            // here says why in terms of the build rather than the archive.
            if (key.toAbsolutePath().normalize()
                    .startsWith(payload.toAbsolutePath().normalize())) {
                throw new MojoExecutionException("the signing key is inside the payload: " + key);
            }

            Map<String, Object> result = cli.json("pack", List.of(
                    payload.toString(),
                    "--config", manifest.toString(),
                    "--key", key.toString(),
                    "--platform", target.id(),
                    "--out-dir", distDirectory.toString()));

            // Read back, never reconstructed. The name is derived from the
            // manifest and sanitised on the way, so anything computed here
            // would be a guess that breaks on the first unusual name.
            getLog().info("xpack: packaged " + Json.string(result, "platform")
                    + "  " + Json.number(result, "files") + " files, "
                    + human(Json.number(result, "size"))
                    + ", signed by " + Json.string(result, "signedBy"));
            Path built = Path.of(Json.string(result, "package"));
            getLog().info("xpack: " + built);

            if (attach) {
                // Classified by platform, because a release publishes one of
                // these per platform and they differ in nothing else a
                // repository can see.
                projectHelper.attachArtifact(project, PACKAGE_TYPE, target.id(), built.toFile());
            }
        }
    }

    /** The artifact type a package is published under. */
    public static final String PACKAGE_TYPE = "xpkg";
}
