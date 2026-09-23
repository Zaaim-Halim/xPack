package io.xpack.internal;

import io.xpack.config.InstallerUiSpec;

/**
 * What {@code xpack:installer} hands the command line about the wizard.
 *
 * <p>Only translation: the command line is the one place these settings are
 * checked, so a mistake reads the same whether the installer was built from
 * Maven or by hand.
 */
public final class InstallerSettings {

    private InstallerSettings() {
    }

    /**
     * The settings file {@code xpack installer --ui} reads.
     *
     * <p>The licence is written as an absolute path. The command line resolves
     * a relative one against the settings file, which here sits in the build
     * directory rather than beside the POM that named it.
     */
    public static String toJson(InstallerUiSpec spec) {
        Json.Obj document = new Json.Obj()
                .putIfAny("pages", spec.getPages())
                .put("license", spec.getLicense() == null
                        ? null
                        : spec.getLicense().getAbsolutePath())
                .putIfAny("text", spec.getText())
                .put("shortcutDefault", spec.getShortcutDefault())
                .put("launchOnFinish", spec.getLaunchOnFinish());
        return document.toString();
    }

    /**
     * The installer build a target is made from when cross-building.
     *
     * <p>The same choice the command line makes for its own host: a Windows
     * installer is the windowed build, which a person double-clicks and which
     * opens the wizard, unless a console one was asked for, for installers
     * only scripts run. Other targets have one build.
     */
    public static String stubName(Target target, boolean console) {
        if (target.os() == Target.Os.WINDOWS && !console) {
            return "xpack-installerw";
        }
        return "xpack-installer";
    }
}
