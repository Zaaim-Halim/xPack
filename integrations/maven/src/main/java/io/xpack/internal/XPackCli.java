package io.xpack.internal;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugin.logging.Log;

/**
 * Runs the xPack command line.
 *
 * <p>Nothing here reimplements what that command line does. Packaging,
 * signing, hashing and the index format have exactly one implementation, and a
 * second one living in a build plugin would be a place for a security fix to
 * be missed.
 */
public final class XPackCli {

    /**
     * Subcommands that accept {@code --json}.
     *
     * <p>Not universal, and passing the flag to a subcommand without it is a
     * hard argument error rather than something quietly ignored, so the two
     * kinds of call are separate methods.
     */
    private static final List<String> MACHINE_READABLE =
            List.of("pack", "delta", "index", "installer", "inspect", "list");

    private final Path executable;
    private final Log log;
    private final long timeoutMinutes;

    public XPackCli(Path executable, Log log, long timeoutMinutes) {
        this.executable = executable;
        this.log = log;
        this.timeoutMinutes = timeoutMinutes;
    }

    /** Whether a subcommand can be asked for machine-readable output. */
    public static boolean isMachineReadable(String subcommand) {
        return MACHINE_READABLE.contains(subcommand);
    }

    public Path executable() {
        return executable;
    }

    /** The version string the binary reports. */
    public String version() throws MojoExecutionException {
        return Processes.run(List.of(executable.toString(), "--version"), log, timeoutMinutes)
                .failOnError("xpack --version")
                .stdout()
                .strip();
    }

    /** Runs a subcommand with {@code --json} and returns the parsed object. */
    public Map<String, Object> json(String subcommand, List<String> arguments)
            throws MojoExecutionException {
        String output = jsonText(subcommand, arguments);
        try {
            return Json.parseObject(output);
        } catch (IllegalArgumentException e) {
            throw unreadable(subcommand, output, e);
        }
    }

    /** As {@link #json}, for a subcommand whose result is an array. */
    public List<?> jsonArray(String subcommand, List<String> arguments)
            throws MojoExecutionException {
        String output = jsonText(subcommand, arguments);
        try {
            Object parsed = Json.parse(output);
            if (!(parsed instanceof List<?> list)) {
                throw new IllegalArgumentException("expected a JSON array");
            }
            return list;
        } catch (IllegalArgumentException e) {
            throw unreadable(subcommand, output, e);
        }
    }

    /**
     * What {@code xpack inspect} says about every package it reads without a
     * key.
     */
    static final String UNVERIFIED_NOTICE = "this manifest has NOT been verified";

    /**
     * Reads a package's manifest, for its platform and version.
     *
     * <p>The plugin asks only about packages it has just built, and trusts
     * nothing it reads here: signatures are checked where they matter, by
     * {@code xpack index} with the public key and by the installer. So the
     * notice {@code xpack inspect} prints for every unverified read would only
     * alarm whoever reads the build log, once per package. Every other line it
     * prints is still passed on.
     */
    public Map<String, Object> inspect(Path pkg) throws MojoExecutionException {
        List<String> command = command("inspect", List.of(pkg.toString()));
        command.add("--json");
        Processes.Result result = Processes.run(command, log, timeoutMinutes);
        result.failOnError("xpack inspect");
        worthShowing(result.stderr()).forEach(log::info);
        try {
            return Json.parseObject(result.stdout());
        } catch (IllegalArgumentException e) {
            throw unreadable("inspect", result.stdout(), e);
        }
    }

    /** The diagnostic lines of an {@link #inspect} worth a reader's attention. */
    static List<String> worthShowing(String stderr) {
        return stderr.lines()
                .filter(line -> !line.isBlank())
                .filter(line -> !line.contains(UNVERIFIED_NOTICE))
                .toList();
    }

    /** Runs a subcommand that has no machine-readable output. */
    public void run(String subcommand, List<String> arguments) throws MojoExecutionException {
        Processes.Result result = Processes.run(command(subcommand, arguments), log, timeoutMinutes);
        result.failOnError("xpack " + subcommand);
        result.logDiagnostics(log);
        if (!result.stdout().isBlank()) {
            result.stdout().lines().forEach(log::info);
        }
    }

    private String jsonText(String subcommand, List<String> arguments)
            throws MojoExecutionException {
        if (!isMachineReadable(subcommand)) {
            throw new MojoExecutionException(
                    "xpack " + subcommand + " has no --json; use run() instead");
        }
        List<String> command = command(subcommand, arguments);
        command.add("--json");
        Processes.Result result = Processes.run(command, log, timeoutMinutes);
        result.failOnError("xpack " + subcommand);
        result.logDiagnostics(log);
        return result.stdout();
    }

    private List<String> command(String subcommand, List<String> arguments) {
        List<String> command = new ArrayList<>();
        command.add(executable.toString());
        command.add(subcommand);
        command.addAll(arguments);
        return command;
    }

    private static MojoExecutionException unreadable(String subcommand, String output,
            Exception cause) {
        return new MojoExecutionException(
                "could not read the output of xpack " + subcommand + ": " + cause.getMessage()
                        + "\n" + output,
                cause);
    }
}
