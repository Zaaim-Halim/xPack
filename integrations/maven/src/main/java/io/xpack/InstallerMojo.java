package io.xpack;

import io.xpack.internal.Json;
import io.xpack.internal.Target;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.LifecyclePhase;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;

/**
 * Builds the artefact a user runs on a machine with no xPack on it.
 *
 * <p>Cross-building needs the target platform's own stub and runtime
 * binaries, because the installer <em>is</em> one of those binaries with a
 * payload attached. There is no flag that substitutes for having them.
 */
@Mojo(name = "installer", defaultPhase = LifecyclePhase.PACKAGE, threadSafe = true)
public class InstallerMojo extends AbstractXPackMojo {

    /**
     * Per-target directories holding that platform's xPack binaries.
     *
     * <p>Keyed by {@code <os>-<arch>}. Needed for any target that is not the
     * host; the host's binaries are found beside the xpack executable.
     */
    @Parameter
    private Map<String, String> targetBinaries = new LinkedHashMap<>();

    /** Leaves the installed version inactive, for an installer that only stages. */
    @Parameter(property = "xpack.installer.noActivate", defaultValue = "false")
    private boolean noActivate;

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }

        List<Path> packages = packages();
        if (packages.isEmpty()) {
            throw new MojoExecutionException(
                    "no .xpkg files in " + distDirectory + "; run xpack:pack first");
        }

        for (Path pkg : packages) {
            Map<String, Object> described = cli().json("inspect", List.of(pkg.toString()));
            Target target = Target.parse(Json.platform(described));

            List<String> arguments = new ArrayList<>();
            arguments.add(pkg.toString());
            arguments.add("--out-dir");
            arguments.add(distDirectory.toString());
            if (noActivate) {
                arguments.add("--no-activate");
            }
            addCrossBuildBinaries(arguments, target);

            Map<String, Object> result = cli().json("installer", arguments);
            getLog().info("xpack: installer for " + Json.string(result, "platform")
                    + "  " + Json.string(result, "layout") + " layout, "
                    + human(Json.number(result, "size")));
        }
    }

    /**
     * Points the build at the target's own binaries when it is not the host.
     *
     * <p>Left alone for the host, where the command line finds the stub and
     * the runtime binaries sitting beside itself.
     */
    private void addCrossBuildBinaries(List<String> arguments, Target target)
            throws MojoExecutionException {
        String configured = targetBinaries.get(target.id());
        if (configured == null || configured.isBlank()) {
            if (!target.equals(Target.host())) {
                throw new MojoExecutionException(
                        "no binaries configured for " + target.id() + ". An installer is that "
                                + "platform's own stub with a payload attached, so building one "
                                + "for a foreign platform needs its binaries:\n"
                                + "  <targetBinaries><" + target.id() + ">/path/to/xpack-"
                                + target.id() + "</" + target.id() + "></targetBinaries>");
            }
            return;
        }

        Path home = Path.of(configured);
        arguments.add("--stub");
        arguments.add(binary(home, "xpack-installer").toString());
        for (String name : List.of("xpack-launcher", "xpack-updater", "xpack-uninstaller")) {
            arguments.add("--binary");
            arguments.add(binary(home, name).toString());
        }
        if (target.os() == Target.Os.WINDOWS) {
            arguments.add("--binary");
            arguments.add(binary(home, "xpack-launcherw").toString());
        }
    }

    private List<Path> packages() throws MojoExecutionException {
        Path dist = distDirectory.toPath();
        if (!Files.isDirectory(dist)) {
            return List.of();
        }
        try (Stream<Path> files = Files.list(dist)) {
            return files.filter(p -> p.getFileName().toString().endsWith(".xpkg")).sorted().toList();
        } catch (IOException e) {
            throw new MojoExecutionException("could not list " + dist, e);
        }
    }
}
