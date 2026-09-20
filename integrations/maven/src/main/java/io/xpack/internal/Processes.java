package io.xpack.internal;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.concurrent.TimeUnit;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugin.logging.Log;

/**
 * Runs an external program and collects what it said.
 *
 * <p>The plugin's whole job is driving other programs — the xPack command
 * line, jlink, jdeps — so the rules for doing that safely live in one place
 * rather than being rediscovered in each goal.
 */
public final class Processes {

    private Processes() {
    }

    /** What a finished process left behind. */
    public static final class Result {
        private final List<String> command;
        private final int exitCode;
        private final String stdout;
        private final String stderr;

        Result(List<String> command, int exitCode, String stdout, String stderr) {
            this.command = command;
            this.exitCode = exitCode;
            this.stdout = stdout;
            this.stderr = stderr;
        }

        public int exitCode() {
            return exitCode;
        }

        public String stdout() {
            return stdout;
        }

        public String stderr() {
            return stderr;
        }

        /** Throws with whichever stream carried the explanation. */
        public Result failOnError(String what) throws MojoExecutionException {
            if (exitCode != 0) {
                String detail = stderr.isBlank() ? stdout : stderr;
                throw new MojoExecutionException(
                        what + " failed with exit code " + exitCode + ":\n" + detail.strip());
            }
            return this;
        }

        /** Reports anything written to standard error, which succeeded anyway. */
        public Result logDiagnostics(Log log) {
            if (!stderr.isBlank()) {
                stderr.lines().forEach(log::info);
            }
            return this;
        }

        @Override
        public String toString() {
            return String.join(" ", command);
        }
    }

    /**
     * Runs a command to completion.
     *
     * <p>Both pipes are drained before waiting. A process whose output fills
     * the pipe buffer blocks writing to it, so waiting first would deadlock on
     * any package large enough to be worth building.
     *
     * <p>The streams are kept apart rather than merged, because results are
     * written to standard output and diagnostics to standard error: merging
     * them would put log lines in front of a JSON document and break parsing
     * the first time anyone raised the log level.
     */
    public static Result run(List<String> command, Log log, long timeoutMinutes)
            throws MojoExecutionException {
        log.debug("running " + String.join(" ", command));
        try {
            Process process = new ProcessBuilder(command).start();
            String stdout = drain(process.getInputStream());
            String stderr = drain(process.getErrorStream());
            if (!process.waitFor(timeoutMinutes, TimeUnit.MINUTES)) {
                process.destroyForcibly();
                throw new MojoExecutionException(
                        command.get(0) + " did not finish within " + timeoutMinutes + " minutes");
            }
            return new Result(command, process.exitValue(), stdout, stderr);
        } catch (IOException e) {
            throw new MojoExecutionException(
                    "could not run " + command.get(0) + ": " + e.getMessage(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new MojoExecutionException("interrupted while running " + command.get(0), e);
        }
    }

    /**
     * Runs a command with the build's own streams, and waits for it.
     *
     * <p>For starting the user's application rather than a tool. Capturing
     * its output would hold every line until it exited, so a developer
     * running a desktop application would watch nothing happen for as long as
     * they left it open, and input would not reach it at all.
     *
     * <p>No timeout, for the same reason: the process ends when the person
     * running it closes the application.
     */
    public static int runInheritingIo(List<String> command, Log log)
            throws MojoExecutionException {
        log.debug("running " + String.join(" ", command));
        try {
            return new ProcessBuilder(command).inheritIO().start().waitFor();
        } catch (IOException e) {
            throw new MojoExecutionException(
                    "could not run " + command.get(0) + ": " + e.getMessage(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new MojoExecutionException("interrupted while running " + command.get(0), e);
        }
    }

    private static String drain(InputStream stream) throws IOException {
        try (stream) {
            return new String(stream.readAllBytes(), StandardCharsets.UTF_8);
        }
    }
}
