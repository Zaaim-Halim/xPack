package io.xpack.config;

import java.util.ArrayList;
import java.util.List;

/** How {@code jlink} should build the runtime bundled into the payload. */
public class RuntimeSpec {

    /**
     * Whether a runtime ships inside the package.
     *
     * <p>True by default, which is what makes an installation independent of
     * what happens to be on the user's machine.
     *
     * <p>Set false for an application that runs on an interpreter the machine
     * already has. The launch executable then becomes a bare {@link #command},
     * which the operating system looks up on {@code PATH} at start — so the
     * application is at the mercy of whatever version it finds, and of
     * whether it is there at all.
     */
    private boolean bundled = true;

    /**
     * The interpreter to start, when no runtime is bundled.
     *
     * <p>A bare name on purpose. Anything with a path separator in it would
     * be read as a file inside the payload, which is the opposite of what
     * this means.
     */
    private String command = "java";

    /** Modules to link. Resolved with jdeps when left empty. */
    private List<String> modules = new ArrayList<>();

    /** Passed straight to {@code jlink --compress}; the spelling is JDK-version specific. */
    private String compress = "zip-6";

    private boolean stripDebug = true;

    private boolean noHeaderFiles = true;

    private boolean noManPages = true;

    /** Extra jlink arguments, for anything this class does not model. */
    private List<String> extraArguments = new ArrayList<>();

    public boolean isBundled() {
        return bundled;
    }

    public void setBundled(boolean bundled) {
        this.bundled = bundled;
    }

    public String getCommand() {
        return command;
    }

    public void setCommand(String command) {
        this.command = command;
    }

    public List<String> getModules() {
        return modules;
    }

    public void setModules(List<String> modules) {
        this.modules = modules;
    }

    public String getCompress() {
        return compress;
    }

    public void setCompress(String compress) {
        this.compress = compress;
    }

    public boolean isStripDebug() {
        return stripDebug;
    }

    public void setStripDebug(boolean stripDebug) {
        this.stripDebug = stripDebug;
    }

    public boolean isNoHeaderFiles() {
        return noHeaderFiles;
    }

    public void setNoHeaderFiles(boolean noHeaderFiles) {
        this.noHeaderFiles = noHeaderFiles;
    }

    public boolean isNoManPages() {
        return noManPages;
    }

    public void setNoManPages(boolean noManPages) {
        this.noManPages = noManPages;
    }

    public List<String> getExtraArguments() {
        return extraArguments;
    }

    public void setExtraArguments(List<String> extraArguments) {
        this.extraArguments = extraArguments;
    }
}
