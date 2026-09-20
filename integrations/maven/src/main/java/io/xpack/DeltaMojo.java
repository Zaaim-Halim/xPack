package io.xpack;

import io.xpack.internal.Json;
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
 * Builds differential updates against an already-published release.
 *
 * <p>A bundled runtime is most of a package and almost never changes, so the
 * difference between two releases of the same application is usually a few
 * jars. A client that cannot use a delta falls back to the full package, which
 * makes publishing one free.
 */
@Mojo(name = "delta", defaultPhase = LifecyclePhase.VERIFY, threadSafe = true)
public class DeltaMojo extends AbstractXPackMojo {

    /**
     * The packages users already have, one per platform.
     *
     * <p>Matched to this build's packages by the platform each declares, so
     * the order does not matter and a missing platform is skipped rather than
     * guessed at.
     */
    @Parameter(property = "xpack.delta.from")
    private List<File> from = new ArrayList<>();

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }
        if (from.isEmpty()) {
            getLog().info("xpack: no <from> packages configured, so no deltas are built");
            return;
        }

        List<Path> current = packages();
        for (File previous : from) {
            Path previousPath = previous.toPath();
            if (!Files.isRegularFile(previousPath)) {
                throw new MojoExecutionException("no package at " + previousPath);
            }
            String platform = platformOf(previousPath);
            Path target = current.stream()
                    .filter(p -> platform.equals(platformOfQuietly(p)))
                    .findFirst()
                    .orElse(null);
            if (target == null) {
                getLog().warn("xpack: nothing built for " + platform
                        + ", so no delta from " + previousPath.getFileName());
                continue;
            }

            Map<String, Object> result = cli().json("delta", List.of(
                    previousPath.toString(), target.toString(),
                    "--out-dir", distDirectory.toString()));

            getLog().info("xpack: delta " + Json.string(result, "from")
                    + " -> " + Json.string(result, "to")
                    + "  " + Json.number(result, "changed") + " changed, "
                    + Json.number(result, "reused") + " reused, "
                    + human(Json.number(result, "size")));
            getLog().info("xpack: " + Json.string(result, "delta"));
        }
    }

    /** Asks the package itself rather than reading its filename. */
    private String platformOf(Path pkg) throws MojoExecutionException {
        return Json.string(cli().json("inspect", List.of(pkg.toString())), "platform");
    }

    private String platformOfQuietly(Path pkg) {
        try {
            return platformOf(pkg);
        } catch (MojoExecutionException e) {
            return null;
        }
    }

    private List<Path> packages() throws MojoExecutionException {
        Path dist = distDirectory.toPath();
        if (!Files.isDirectory(dist)) {
            throw new MojoExecutionException("no packages in " + dist + "; run xpack:pack first");
        }
        try (Stream<Path> files = Files.list(dist)) {
            return files.filter(p -> p.getFileName().toString().endsWith(".xpkg")).sorted().toList();
        } catch (IOException e) {
            throw new MojoExecutionException("could not list " + dist, e);
        }
    }
}
