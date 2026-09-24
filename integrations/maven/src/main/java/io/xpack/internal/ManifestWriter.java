package io.xpack.internal;

import io.xpack.config.DesktopSpec;
import io.xpack.config.HealthSpec;
import io.xpack.config.PromptSpec;
import io.xpack.config.UpdateSpec;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.regex.Pattern;

/**
 * Builds the {@code xpack.json} the command line reads.
 *
 * <p>Kept free of Maven types so the mapping can be tested by asserting on the
 * text, which is the part that has to be exactly right.
 *
 * <p>The payload inventory is deliberately absent. It is computed from the
 * files actually on disk by the tool that writes the archive, because a
 * manifest disagreeing with its own archive would fail verification on every
 * client.
 */
public final class ManifestWriter {

    /** Anything a semantic version parser will accept. */
    private static final Pattern SEMVER = Pattern.compile(
            "^(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)"
                    + "(?:-((?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*)"
                    + "(?:\\.(?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?"
                    + "(?:\\+([0-9a-zA-Z-]+(?:\\.[0-9a-zA-Z-]+)*))?$");

    private static final Pattern NUMERIC = Pattern.compile("0|[1-9]\\d*");


    private String id;
    private String name;
    private String version;
    private String description;
    private String publisher;
    private Target target;
    private String executable;
    private List<String> arguments = new ArrayList<>();
    private Map<String, String> environment = new LinkedHashMap<>();
    private boolean keepWorkingDirectory;
    private String updateUrl;
    private UpdateSpec update = new UpdateSpec();
    private HealthSpec health;
    private DesktopSpec desktop;
    private String command;

    public ManifestWriter id(String value) {
        this.id = value;
        return this;
    }

    public ManifestWriter name(String value) {
        this.name = value;
        return this;
    }

    public ManifestWriter version(String value) {
        this.version = value;
        return this;
    }

    public ManifestWriter description(String value) {
        this.description = blankToNull(value);
        return this;
    }

    public ManifestWriter publisher(String value) {
        this.publisher = blankToNull(value);
        return this;
    }

    public ManifestWriter target(Target value) {
        this.target = value;
        return this;
    }

    public ManifestWriter executable(String value) {
        this.executable = value;
        return this;
    }

    public ManifestWriter arguments(List<String> value) {
        this.arguments = value == null ? new ArrayList<>() : value;
        return this;
    }

    /** Whether the application starts where it was run from; {@code null} is off. */
    public ManifestWriter keepWorkingDirectory(Boolean value) {
        this.keepWorkingDirectory = Boolean.TRUE.equals(value);
        return this;
    }

    public ManifestWriter environment(Map<String, String> value) {
        this.environment = value == null ? new LinkedHashMap<>() : value;
        return this;
    }

    /**
     * The update settings, with {@code url} already resolved for this target.
     *
     * <p>The URL is passed separately because it is derived rather than
     * configured: the plugin holds a base, and the caller appends the segment
     * naming the platform this manifest is for.
     *
     * <p>Nothing here is validated. What a severity may say, how short an
     * interval may be, how long a prompt may run -- the packager refuses a
     * manifest that breaks any of those, and it names the line, the column and
     * the words it will accept. A second copy of the rules in this file would
     * add a second thing to keep in step, and would be the copy that is wrong
     * first.
     */
    public ManifestWriter update(String url, UpdateSpec spec) {
        this.updateUrl = blankToNull(url);
        this.update = spec == null ? new UpdateSpec() : spec;
        return this;
    }

    public ManifestWriter health(HealthSpec value) {
        this.health = value;
        return this;
    }

    public ManifestWriter desktop(DesktopSpec value) {
        this.desktop = value;
        return this;
    }

    /** The command a terminal starts the application by; blank is none. */
    public ManifestWriter command(String value) {
        this.command = blankToNull(value);
        return this;
    }

    /**
     * Turns a Maven version into one a semantic version parser accepts.
     *
     * <p>{@code 1.2.0-SNAPSHOT} is already valid and sorts below {@code 1.2.0},
     * which is what a snapshot should do. {@code 1.2-SNAPSHOT} and {@code 1.2}
     * are not valid at all, so the missing component is filled in here rather
     * than left to fail later with a message about a file the user never
     * wrote.
     */
    public static String normaliseVersion(String mavenVersion) {
        String value = mavenVersion == null ? "" : mavenVersion.trim();
        if (value.isEmpty()) {
            throw new IllegalArgumentException("the project has no version");
        }
        if (SEMVER.matcher(value).matches()) {
            return value;
        }

        String core = value;
        String suffix = "";
        int dash = value.indexOf('-');
        if (dash >= 0) {
            core = value.substring(0, dash);
            suffix = value.substring(dash);
        }

        String[] parts = core.split("\\.", -1);
        if (parts.length > 3) {
            throw new IllegalArgumentException(explain(mavenVersion));
        }
        StringBuilder padded = new StringBuilder();
        for (int i = 0; i < 3; i++) {
            if (i > 0) {
                padded.append('.');
            }
            String part = i < parts.length ? parts[i] : "0";
            if (!NUMERIC.matcher(part).matches()) {
                throw new IllegalArgumentException(explain(mavenVersion));
            }
            padded.append(part);
        }
        String candidate = padded + suffix;
        if (!SEMVER.matcher(candidate).matches()) {
            throw new IllegalArgumentException(explain(mavenVersion));
        }
        return candidate;
    }

    private static String explain(String mavenVersion) {
        return "version \"" + mavenVersion + "\" cannot be read as a semantic version; "
                + "set <version> on the plugin, or -Dxpack.version=";
    }

    /** Renders the manifest. */
    public String toJson() {
        require(id, "application id");
        require(name, "application name");
        require(version, "application version");
        require(executable, "launch executable");
        if (target == null) {
            throw new IllegalStateException("no target platform");
        }

        Json.Obj application = new Json.Obj()
                .put("id", id)
                .put("name", name)
                .put("version", normaliseVersion(version))
                .put("description", description)
                .put("publisher", publisher);

        Json.Obj platform = new Json.Obj()
                .put("os", target.os().id())
                .put("arch", target.arch().id());

        // workingDirectory is deliberately never written. The version
        // directory is the default and "." is refused, so emitting the value
        // that looks like the default produces a manifest that will not load.
        // Written only when on: a file without it is the one every earlier
        // build produced, which `xpack pack` still packs as the oldest format.
        Json.Obj launch = new Json.Obj()
                .put("executable", executable)
                .putIfAny("arguments", arguments)
                .put("keepWorkingDirectory", keepWorkingDirectory ? Boolean.TRUE : null)
                .putIfAny("environment", environment);

        Json.Obj updateObject = new Json.Obj()
                .put("channel", blankToNull(update.getChannel()))
                .put("url", updateUrl)
                .put("mandatory", update.getMandatory())
                .put("checkWhileRunning", update.getCheckWhileRunning())
                .put("checkIntervalMinutes", update.getCheckIntervalMinutes())
                .put("notify", update.getNotify())
                .put("severity", blankToNull(update.getSeverity()));

        PromptSpec prompt = update.getPrompt();
        if (prompt != null && !prompt.isEmpty()) {
            updateObject.put(
                    "prompt",
                    new Json.Obj()
                            .put("title", blankToNull(prompt.getTitle()))
                            .put("message", blankToNull(prompt.getMessage())));
        }

        Json.Obj healthObject = new Json.Obj();
        if (health != null && !health.isEmpty()) {
            healthObject
                    .put("startupTimeoutSeconds", health.getStartupTimeoutSeconds())
                    .put("requireStartupReport", health.getRequireStartupReport());
        }

        Json.Obj desktopObject = new Json.Obj();
        if (desktop != null && !desktop.isEmpty()) {
            desktopObject
                    .put("shortcut", desktop.getShortcut())
                    .put("icon", desktop.getIcon())
                    .putIfAny("categories", desktop.getCategories())
                    .put("terminal", desktop.getTerminal());
        }

        return new Json.Obj()
                .put("application", application)
                .put("platform", platform)
                .put("launch", launch)
                .putIfAny("update", updateObject)
                .putIfAny("health", healthObject)
                .putIfAny("desktop", desktopObject)
                .putIfAny("command", new Json.Obj().put("name", command))
                .toString();
    }

    private static void require(String value, String what) {
        if (value == null || value.isBlank()) {
            throw new IllegalStateException("no " + what);
        }
    }

    private static String blankToNull(String value) {
        return value == null || value.isBlank() ? null : value;
    }
}
