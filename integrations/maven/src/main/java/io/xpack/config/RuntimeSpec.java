package io.xpack.config;

import java.util.ArrayList;
import java.util.List;

/** How {@code jlink} should build the runtime bundled into the payload. */
public class RuntimeSpec {

    /** Modules to link. Resolved with jdeps when left empty. */
    private List<String> modules = new ArrayList<>();

    /** Passed straight to {@code jlink --compress}; the spelling is JDK-version specific. */
    private String compress = "zip-6";

    private boolean stripDebug = true;

    private boolean noHeaderFiles = true;

    private boolean noManPages = true;

    /** Extra jlink arguments, for anything this class does not model. */
    private List<String> extraArguments = new ArrayList<>();

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
