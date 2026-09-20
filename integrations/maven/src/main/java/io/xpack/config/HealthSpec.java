package io.xpack.config;

/**
 * The startup window a new version has to survive before it is trusted.
 *
 * <p>Creating the file named by {@code XPACK_HEALTH_FILE} is the only signal
 * that is a fact rather than an inference, which is what
 * {@code requireStartupReport} insists on.
 */
public class HealthSpec {

    private Integer startupTimeoutSeconds;

    private Boolean requireStartupReport;

    public Integer getStartupTimeoutSeconds() {
        return startupTimeoutSeconds;
    }

    public void setStartupTimeoutSeconds(Integer startupTimeoutSeconds) {
        this.startupTimeoutSeconds = startupTimeoutSeconds;
    }

    public Boolean getRequireStartupReport() {
        return requireStartupReport;
    }

    public void setRequireStartupReport(Boolean requireStartupReport) {
        this.requireStartupReport = requireStartupReport;
    }

    public boolean isEmpty() {
        return startupTimeoutSeconds == null && requireStartupReport == null;
    }
}
