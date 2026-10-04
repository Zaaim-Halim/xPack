package io.xpack.config;

/**
 * One hook: a script of the project's hooks directory, the moment it runs at,
 * and how long it may take.
 *
 * <p>{@code script} names a file under the hooks directory
 * ({@code src/xpack/hooks} unless configured), which the plugin copies into
 * the payload at {@code xpack/hooks/}. {@code when} is the moment it runs
 * at, which each operation limits:
 *
 * <ul>
 *   <li>install: {@code before}, {@code afterFiles} or {@code after};
 *   <li>update: {@code before}, {@code after} or {@code confirmed};
 *   <li>rollback and uninstall: {@code before} or {@code after}.
 * </ul>
 *
 * <p>Left out, {@code after}, except for uninstall, where it is
 * {@code before}, while the application is still there. Nothing here is
 * checked: {@code xpack pack} checks every hook and names what is wrong.
 */
public class HookSpec {

    private String script;

    private String when;

    private Integer timeoutSeconds;

    public String getScript() {
        return script;
    }

    public void setScript(String script) {
        this.script = script;
    }

    public String getWhen() {
        return when;
    }

    public void setWhen(String when) {
        this.when = when;
    }

    public Integer getTimeoutSeconds() {
        return timeoutSeconds;
    }

    public void setTimeoutSeconds(Integer timeoutSeconds) {
        this.timeoutSeconds = timeoutSeconds;
    }
}
