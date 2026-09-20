package io.xpack.config;

import java.util.ArrayList;
import java.util.List;

/** The Start Menu entry, {@code .desktop} file or {@code .app} bundle to create. */
public class DesktopSpec {

    private Boolean shortcut;

    /** Payload-relative, and the file must exist in the payload. */
    private String icon;

    /** freedesktop categories; Linux only. */
    private List<String> categories = new ArrayList<>();

    private Boolean terminal;

    public Boolean getShortcut() {
        return shortcut;
    }

    public void setShortcut(Boolean shortcut) {
        this.shortcut = shortcut;
    }

    public String getIcon() {
        return icon;
    }

    public void setIcon(String icon) {
        this.icon = icon;
    }

    public List<String> getCategories() {
        return categories;
    }

    public void setCategories(List<String> categories) {
        this.categories = categories;
    }

    public Boolean getTerminal() {
        return terminal;
    }

    public void setTerminal(Boolean terminal) {
        this.terminal = terminal;
    }

    public boolean isEmpty() {
        return shortcut == null && icon == null && terminal == null
                && (categories == null || categories.isEmpty());
    }
}
