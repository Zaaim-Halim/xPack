package io.xpack;

import io.xpack.config.DesktopSpec;
import io.xpack.config.HealthSpec;
import io.xpack.config.RuntimeSpec;
import io.xpack.internal.Json;
import io.xpack.internal.Layout;
import io.xpack.internal.ManifestWriter;
import io.xpack.internal.Target;
import io.xpack.internal.XPackCli;
import java.io.File;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.apache.maven.plugin.AbstractMojo;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.Parameter;
import org.apache.maven.project.MavenProject;

/** Configuration and helpers every xPack goal shares. */
public abstract class AbstractXPackMojo extends AbstractMojo {

    @Parameter(defaultValue = "${project}", readonly = true, required = true)
    protected MavenProject project;

    /** Skips every xPack goal. */
    @Parameter(property = "xpack.skip", defaultValue = "false")
    protected boolean skip;

    // ------------------------------------------------------------- the binary

    /** Directory holding the xPack binaries. Overrides {@code XPACK_HOME}. */
    @Parameter(property = "xpack.home")
    protected File xpackHome;

    /** The {@code xpack} binary itself, when it is not in {@link #xpackHome}. */
    @Parameter(property = "xpack.executable")
    protected File xpackExecutable;

    /** How long any single command line invocation may take. */
    @Parameter(property = "xpack.timeoutMinutes", defaultValue = "30")
    protected long timeoutMinutes;

    // --------------------------------------------------------------- signing

    /** The private signing key. Never put this in the POM; pass a path. */
    @Parameter(property = "xpack.key")
    protected File signingKey;

    /** The matching public key, needed by goals that install or index. */
    @Parameter(property = "xpack.publicKey")
    protected File publicKey;

    /** Overrides the version taken from the project. */
    @Parameter(property = "xpack.version")
    protected String version;

    // -------------------------------------------------------------- identity

    /** Reverse-DNS application id. Defaults to groupId.artifactId. */
    @Parameter(property = "xpack.id")
    protected String id;

    /** Display name. Defaults to the project name. */
    @Parameter(property = "xpack.name")
    protected String name;

    @Parameter(property = "xpack.description", defaultValue = "${project.description}")
    protected String description;

    @Parameter(property = "xpack.publisher")
    protected String publisher;

    // ---------------------------------------------------------------- launch

    /** Main class of the application being packaged. */
    @Parameter(property = "xpack.mainClass")
    protected String mainClass;

    /** Arguments passed to the JVM, before the class path. */
    @Parameter
    protected List<String> jvmArgs = new ArrayList<>();

    /** Arguments passed to the application. */
    @Parameter
    protected List<String> appArgs = new ArrayList<>();

    /** Environment applied over the inherited environment. */
    @Parameter
    protected Map<String, String> environment = new LinkedHashMap<>();

    // --------------------------------------------------------------- targets

    /** Platforms to build, as {@code <os>-<arch>}. Defaults to the host. */
    @Parameter(property = "xpack.targets")
    protected List<String> targets = new ArrayList<>();

    // --------------------------------------------------------------- runtime

    @Parameter
    protected RuntimeSpec runtime = new RuntimeSpec();

    /**
     * JDKs to link each target's runtime from, keyed by {@code <os>-<arch>}.
     *
     * <p>A target with no entry is linked from the JDK running the build,
     * which is only correct when that target is the host.
     */
    @Parameter
    protected Map<String, String> targetJdks = new LinkedHashMap<>();

    /** Name the runtime takes inside the payload. */
    @Parameter(property = "xpack.runtimeDirectory", defaultValue = "runtime")
    protected String runtimeDirectory;

    // --------------------------------------------------------------- updates

    /** Base update URL. The {@code <os>-<arch>} segment is appended per target. */
    @Parameter(property = "xpack.updateBaseUrl")
    protected String updateBaseUrl;

    @Parameter(property = "xpack.updateChannel")
    protected String updateChannel;

    @Parameter(property = "xpack.mandatory")
    protected Boolean mandatory;

    // -------------------------------------------------------- health, desktop

    @Parameter
    protected HealthSpec health = new HealthSpec();

    @Parameter
    protected DesktopSpec desktop = new DesktopSpec();

    // --------------------------------------------------------------- outputs

    /** Working directory for everything this plugin builds. */
    @Parameter(defaultValue = "${project.build.directory}/xpack", readonly = true)
    protected File workDirectory;

    /** Where finished packages and installers are written. */
    @Parameter(property = "xpack.distDirectory",
            defaultValue = "${project.build.directory}/xpack/dist")
    protected File distDirectory;

    // --------------------------------------------------------------- helpers

    protected Layout layout() {
        return new Layout(workDirectory.toPath(), runtimeDirectory);
    }

    /** Resolves the {@code xpack} binary, or explains how to point at one. */
    protected Path executable() throws MojoExecutionException {
        if (xpackExecutable != null) {
            return requireExecutable(xpackExecutable.toPath());
        }
        if (xpackHome != null) {
            return requireExecutable(binary(xpackHome.toPath(), "xpack"));
        }
        String home = System.getenv("XPACK_HOME");
        if (home != null && !home.isBlank()) {
            return requireExecutable(binary(Paths.get(home), "xpack"));
        }
        // Left as a bare name so the operating system searches PATH, which is
        // the same rule a manifest's launch executable follows.
        return Paths.get(Target.host().executableName("xpack"));
    }

    /** A named binary inside an xPack distribution directory. */
    protected Path binary(Path home, String base) {
        String executableName = Target.host().executableName(base);
        Path direct = home.resolve(executableName);
        if (Files.isRegularFile(direct)) {
            return direct;
        }
        return home.resolve("bin").resolve(executableName);
    }

    private Path requireExecutable(Path path) throws MojoExecutionException {
        if (!Files.isRegularFile(path)) {
            throw new MojoExecutionException("no xpack binary at " + path);
        }
        return path;
    }

    protected XPackCli cli() throws MojoExecutionException {
        return new XPackCli(executable(), getLog(), timeoutMinutes);
    }

    /** The platforms to build for. */
    protected List<Target> targets() throws MojoExecutionException {
        if (targets == null || targets.isEmpty()) {
            return List.of(Target.host());
        }
        List<Target> parsed = new ArrayList<>();
        for (String target : targets) {
            try {
                parsed.add(Target.parse(target));
            } catch (IllegalArgumentException e) {
                throw new MojoExecutionException(e.getMessage(), e);
            }
        }
        return parsed;
    }

    protected String applicationId() {
        if (id != null && !id.isBlank()) {
            return id;
        }
        return project.getGroupId() + "." + project.getArtifactId();
    }

    protected String applicationName() {
        if (name != null && !name.isBlank()) {
            return name;
        }
        if (project.getName() != null && !project.getName().isBlank()) {
            return project.getName();
        }
        return project.getArtifactId();
    }

    /**
     * The version this build is releasing, as a manifest would spell it.
     *
     * <p>Normalised, because a Maven version is not always a semantic one and
     * the manifest carries the normalised form. Comparing an artefact against
     * the raw project version would find nothing for a project at `1.2`.
     */
    protected String releaseVersion() throws MojoExecutionException {
        try {
            return ManifestWriter.normaliseVersion(
                    version != null && !version.isBlank() ? version : project.getVersion());
        } catch (IllegalArgumentException e) {
            throw new MojoExecutionException(e.getMessage(), e);
        }
    }

    /** One artefact in the distribution directory, and what it says it is. */
    protected record Artefact(Path file, String platform, String version) {}

    /**
     * Artefacts belonging to *this* release, by what each one declares.
     *
     * <p>The distribution directory is not emptied between builds, so after
     * releasing 1.1.0 it still holds 1.0.0's package from the last one.
     * Publishing both would be refused — two packages cannot be the same
     * release on the same channel — and choosing between them by filename
     * would be a guess. Each is asked instead.
     *
     * <p>Not filtered by deleting the older ones: the directory is
     * configurable, so a build could be pointed at somewhere holding a
     * publisher's whole release history, and clearing that would be a
     * catastrophe caused by a default.
     *
     * @param suffix {@code .xpkg} for packages, {@code .xpkgd} for deltas
     */
    protected List<Artefact> releaseArtefacts(String suffix) throws MojoExecutionException {
        Path dist = distDirectory.toPath();
        if (!Files.isDirectory(dist)) {
            return List.of();
        }

        List<Path> candidates;
        try (var files = Files.list(dist)) {
            candidates = files.filter(p -> p.getFileName().toString().endsWith(suffix))
                    .sorted()
                    .toList();
        } catch (java.io.IOException e) {
            throw new MojoExecutionException("could not list " + dist, e);
        }

        String wantedVersion = releaseVersion();
        String wantedApplication = applicationId();
        List<Artefact> mine = new ArrayList<>();
        for (Path candidate : candidates) {
            Map<String, Object> described =
                    cli().json("inspect", List.of(candidate.toString()));
            Map<String, Object> application = Json.object(described, "application");
            if (!wantedApplication.equals(Json.string(application, "id"))) {
                continue;
            }
            String found = Json.string(application, "version");
            if (!wantedVersion.equals(found)) {
                getLog().debug("xpack: ignoring " + candidate.getFileName()
                        + ", which is version " + found);
                continue;
            }
            mine.add(new Artefact(candidate, Json.platform(described), found));
        }
        return mine;
    }

    protected Path signingKeyPath() throws MojoExecutionException {
        if (signingKey != null) {
            return signingKey.toPath();
        }
        String fromEnvironment = System.getenv("XPACK_SIGNING_KEY");
        if (fromEnvironment != null && !fromEnvironment.isBlank()) {
            return Paths.get(fromEnvironment);
        }
        throw new MojoExecutionException(
                "no signing key. Pass -Dxpack.key=<path> or set XPACK_SIGNING_KEY. "
                        + "Generate one with: xpack keygen --out xpack-signing.json");
    }

    /** The jar this project built, which becomes the application payload. */
    protected Path projectArtifact() throws MojoExecutionException {
        if (project.getArtifact() != null && project.getArtifact().getFile() != null) {
            return project.getArtifact().getFile().toPath();
        }
        Path guess = Path.of(project.getBuild().getDirectory())
                .resolve(project.getBuild().getFinalName() + ".jar");
        if (Files.isRegularFile(guess)) {
            return guess;
        }
        throw new MojoExecutionException(
                "this project has produced no jar yet; bind the xpack goals to package "
                        + "or later, and run a phase that builds one");
    }

    /** Deletes a tree, so a rebuild does not inherit the last one's files. */
    protected static void deleteRecursively(Path root) throws MojoExecutionException {
        if (!Files.exists(root, java.nio.file.LinkOption.NOFOLLOW_LINKS)) {
            return;
        }
        try (var paths = Files.walk(root)) {
            for (Path path : paths.sorted(java.util.Comparator.reverseOrder()).toList()) {
                Files.deleteIfExists(path);
            }
        } catch (java.io.IOException e) {
            throw new MojoExecutionException("could not clear " + root, e);
        }
    }

    /** Bytes, for a human reading a build log. */
    protected static String human(long bytes) {
        if (bytes < 1024) {
            return bytes + " B";
        }
        String[] units = {"KiB", "MiB", "GiB"};
        double value = bytes / 1024.0;
        int unit = 0;
        while (value >= 1024 && unit < units.length - 1) {
            value /= 1024;
            unit++;
        }
        return String.format("%.1f %s", value, units[unit]);
    }
}
