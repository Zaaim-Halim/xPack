package io.xpack.config;

import java.io.File;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * How the installation wizard looks: which pages appear, the licence it
 * shows, and the few lines a publisher may reword.
 *
 * <p>Every setting has a default, and so does the block: leaving it out gives
 * the recommended wizard. Branding is not here; the name, publisher and icon
 * come from the signed package. Checked by the xPack command line, which
 * fails the build on anything it does not accept.
 */
public class InstallerUiSpec {

    /** Which pages appear, by name; their order is fixed. */
    private List<String> pages = new ArrayList<>();

    /** A UTF-8 plain-text licence file. Without one there is no licence page. */
    private File license;

    /** Replacements for the {@code welcome} and {@code finish} lines. */
    private Map<String, String> text = new LinkedHashMap<>();

    /** Whether the desktop-entry box starts ticked, where it is offered. */
    private Boolean shortcutDefault;

    /** Whether the command-line box starts ticked, where it is offered. */
    private Boolean pathDefault;

    /** Whether the last page offers to open the application. */
    private Boolean launchOnFinish;

    public List<String> getPages() {
        return pages;
    }

    public void setPages(List<String> pages) {
        this.pages = pages;
    }

    public File getLicense() {
        return license;
    }

    public void setLicense(File license) {
        this.license = license;
    }

    public Map<String, String> getText() {
        return text;
    }

    public void setText(Map<String, String> text) {
        this.text = text;
    }

    public Boolean getShortcutDefault() {
        return shortcutDefault;
    }

    public void setShortcutDefault(Boolean shortcutDefault) {
        this.shortcutDefault = shortcutDefault;
    }

    public Boolean getPathDefault() {
        return pathDefault;
    }

    public void setPathDefault(Boolean pathDefault) {
        this.pathDefault = pathDefault;
    }

    public Boolean getLaunchOnFinish() {
        return launchOnFinish;
    }

    public void setLaunchOnFinish(Boolean launchOnFinish) {
        this.launchOnFinish = launchOnFinish;
    }

    public boolean isEmpty() {
        return (pages == null || pages.isEmpty())
                && license == null
                && (text == null || text.isEmpty())
                && shortcutDefault == null
                && pathDefault == null
                && launchOnFinish == null;
    }
}
