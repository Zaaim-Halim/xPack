package io.xpack.config;

/**
 * Whether and how often to look for an update, and what to say about one.
 *
 * <p>Every field is optional, and an installation whose manifest says nothing
 * here behaves as it always has: it checks when the application starts and
 * applies what it finds at the next start, in silence.
 *
 * <p>The update URL is not here. It is the one setting that routinely differs
 * between a developer's build and a release build, and a command-line override
 * needs a parameter on the plugin itself -- Maven reads {@code property = }
 * only there, never on a nested block like this one. Carrying it in both
 * places would mean a rule about which wins, and a rule like that is answered
 * by reading the source rather than by reading the POM.
 */
public class UpdateSpec {

    private String channel;

    private Boolean mandatory;

    /**
     * Whether to keep looking for updates while the application is open.
     *
     * <p>Off unless asked for. The application is checked when it starts, and
     * whatever that check finds is applied at the next start; nothing runs in
     * between. On, the launcher keeps looking for as long as the application
     * is open -- which is still not a promise that anything is checked on a
     * schedule, because an application nobody opens is never checked.
     *
     * <p>A later release can turn this on or off freely: the checking is done
     * by binaries every installation already has.
     */
    private Boolean checkWhileRunning;

    /**
     * Shortest time between asking the update server anything, in minutes.
     *
     * <p>Governs every check, not only the ones made while the application
     * runs. Unset means four hours; anything below fifteen minutes is raised
     * to fifteen.
     *
     * <p>It says how often, never whether -- leaving it out cannot turn
     * checking off. {@code checkWhileRunning} is the only thing that does.
     */
    private Integer checkIntervalMinutes;

    /**
     * Whether a check that finds an update may tell the user about it.
     *
     * <p>Off unless asked for. A prompt interrupts somebody, and a publisher
     * who has not asked for one has not agreed to interrupt their users.
     *
     * <p><strong>This one an update cannot turn on.</strong> Showing a dialog
     * needs a binary in the installation, and only an installer can place one
     * -- a background update runs from inside the installation with no copy to
     * give it. Setting this in a later release changes nothing for anybody who
     * installed before it; they need a new installer. The installer places the
     * dialog when this and {@code checkWhileRunning} are both on and the
     * target has one implemented, which today means Windows and macOS.
     */
    private Boolean notify;

    /**
     * How urgent the release is: {@code optional}, {@code recommended} or
     * {@code critical}.
     *
     * <p>Decides what the prompt offers, not what is installed. A critical
     * release is told rather than offered.
     */
    private String severity;

    private PromptSpec prompt;

    public String getChannel() {
        return channel;
    }

    public void setChannel(String channel) {
        this.channel = channel;
    }

    public Boolean getMandatory() {
        return mandatory;
    }

    public void setMandatory(Boolean mandatory) {
        this.mandatory = mandatory;
    }

    public Boolean getCheckWhileRunning() {
        return checkWhileRunning;
    }

    public void setCheckWhileRunning(Boolean checkWhileRunning) {
        this.checkWhileRunning = checkWhileRunning;
    }

    public Integer getCheckIntervalMinutes() {
        return checkIntervalMinutes;
    }

    public void setCheckIntervalMinutes(Integer checkIntervalMinutes) {
        this.checkIntervalMinutes = checkIntervalMinutes;
    }

    public Boolean getNotify() {
        return notify;
    }

    public void setNotify(Boolean notify) {
        this.notify = notify;
    }

    public String getSeverity() {
        return severity;
    }

    public void setSeverity(String severity) {
        this.severity = severity;
    }

    public PromptSpec getPrompt() {
        return prompt;
    }

    public void setPrompt(PromptSpec prompt) {
        this.prompt = prompt;
    }
}
