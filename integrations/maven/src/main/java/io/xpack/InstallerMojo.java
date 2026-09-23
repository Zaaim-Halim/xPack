package io.xpack;

import io.xpack.config.InstallerUiSpec;
import io.xpack.internal.InstallerSettings;
import io.xpack.internal.Json;
import io.xpack.internal.Target;
import java.io.IOException;
import java.io.File;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;

/**
 * Builds the artefact a user runs on a machine with no xPack on it.
 *
 * <p>Cross-building needs the target platform's own stub and runtime
 * binaries, because the installer <em>is</em> one of those binaries with a
 * payload attached. There is no flag that substitutes for having them.
 */
// No default phase on purpose. An installer carries the whole package plus
// the runtime binaries and changes almost never, so building one on every
// commit is tens of megabytes of nothing. Binding it means naming a phase.
@Mojo(name = "installer", threadSafe = true)
public class InstallerMojo extends AbstractXPackMojo {

    /**
     * Per-target directories holding that platform's xPack binaries.
     *
     * <p>Keyed by {@code <os>-<arch>}. Needed for any target that is not the
     * host; the host's binaries are found beside the xpack executable.
     */
    @Parameter
    private Map<String, String> targetBinaries = new LinkedHashMap<>();

    /**
     * Deprecated: the icon now comes from {@code <desktop><icon>}.
     *
     * <p>The command line takes the installer's icon from the package's own
     * desktop icon, the one source every other surface uses. This is still
     * passed on, and still wins, so existing builds are unchanged; it logs a
     * warning saying so.
     */
    @Parameter(property = "xpack.icon")
    private File icon;

    /**
     * How the installation wizard looks. Every setting has a default, and so
     * does the block.
     */
    @Parameter
    private InstallerUiSpec installerUi;

    /**
     * Builds a Windows installer on the console build instead of the windowed
     * one.
     *
     * <p>The windowed build is the default: it is what a person double-clicks,
     * and it opens the wizard. A shell does not wait for a windowed program,
     * though, so an installer only scripts run is better built on the console
     * one. Other targets have one build and ignore this.
     */
    @Parameter(property = "xpack.installer.console", defaultValue = "false")
    private boolean console;

    /** Leaves the installed version inactive, for an installer that only stages. */
    @Parameter(property = "xpack.installer.noActivate", defaultValue = "false")
    private boolean noActivate;

    @Override
    public void execute() throws MojoExecutionException {
        if (skip) {
            getLog().info("xpack: skipped");
            return;
        }

        List<Artefact> packages = releaseArtefacts(".xpkg");
        if (packages.isEmpty()) {
            throw new MojoExecutionException("no packages for " + releaseVersion() + " in "
                    + distDirectory + "; run xpack:pack first");
        }

        for (Artefact artefact : packages) {
            Path pkg = artefact.file();
            Target target = Target.parse(artefact.platform());

            List<String> arguments = new ArrayList<>();
            arguments.add(pkg.toString());
            arguments.add("--out-dir");
            arguments.add(distDirectory.toString());
            if (noActivate) {
                arguments.add("--no-activate");
            }
            if (icon != null) {
                if (!icon.isFile()) {
                    throw new MojoExecutionException("no icon at " + icon);
                }
                getLog().warn("xpack: <icon> is deprecated; the installer's icon now comes from "
                        + "<desktop><icon>. It is still used because it was given.");
                arguments.add("--icon");
                arguments.add(icon.toString());
            }
            if (console) {
                arguments.add("--console");
            }
            Path settings = writeInstallerSettings();
            if (settings != null) {
                arguments.add("--ui");
                arguments.add(settings.toString());
            }
            addCrossBuildBinaries(arguments, target);

            Map<String, Object> result = cli().json("installer", arguments);
            getLog().info("xpack: installer for " + Json.string(result, "platform")
                    + "  " + Json.string(result, "layout") + " layout, "
                    + human(Json.number(result, "size")));
        }
    }

    /**
     * Writes the wizard settings for the command line, when there are any.
     *
     * <p>None when {@code <installerUi>} is absent or empty, so an installer
     * built without it is exactly what it was before the block existed.
     */
    private Path writeInstallerSettings() throws MojoExecutionException {
        if (installerUi == null || installerUi.isEmpty()) {
            return null;
        }
        Path file = Path.of(project.getBuild().getDirectory(), "xpack", "installer-ui.json");
        try {
            Files.createDirectories(file.getParent());
            Files.writeString(file, InstallerSettings.toJson(installerUi));
        } catch (IOException e) {
            throw new MojoExecutionException("cannot write " + file + ": " + e.getMessage(), e);
        }
        return file;
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
        arguments.add(binary(home, InstallerSettings.stubName(target, console)).toString());
        for (String name : List.of("xpack-launcher", "xpack-updater", "xpack-uninstaller")) {
            arguments.add("--binary");
            arguments.add(binary(home, name).toString());
        }
        if (target.os() == Target.Os.WINDOWS) {
            arguments.add("--binary");
            arguments.add(binary(home, "xpack-launcherw").toString());
        }
    }
}
