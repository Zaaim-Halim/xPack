package io.xpack.internal;

import io.xpack.config.InstallerUiSpec;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

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
                .put("pathDefault", spec.getPathDefault())
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

    /**
     * The runtime binaries a cross-built installer carries, by name.
     *
     * <p>The same list the command line gathers for its own host: launcher,
     * updater and uninstaller; the windowed launcher for Windows; and the
     * update notice when the package asks for it (see {@link #wantsNotice}).
     * Only an installer can place the notice, so leaving it out here would
     * make every update of such an application silent.
     */
    public static List<String> runtimeBinaries(Target target, boolean notice) {
        List<String> names = new ArrayList<>(List.of("xpack-launcher", "xpack-updater", "xpack-uninstaller"));
        if (target.os() == Target.Os.WINDOWS) {
            names.add("xpack-launcherw");
        }
        if (notice) {
            names.add("xpack-notify");
        }
        return names;
    }

    /**
     * Whether an installer for this package must carry the update notice.
     *
     * <p>The rule installing applies: the package asks to announce updates
     * ({@code update.notify}), checks for them while it runs
     * ({@code update.checkWhileRunning}, the only check that can find one to
     * announce), and targets a platform xPack has a dialog for, macOS or
     * Windows.
     *
     * @param manifest the package's manifest, as {@code xpack inspect --json}
     *     reports it
     */
    public static boolean wantsNotice(Map<String, Object> manifest) {
        Object update = manifest.get("update");
        if (!(update instanceof Map<?, ?> spec)) {
            return false;
        }
        Target.Os os = Target.parse(Json.platform(manifest)).os();
        return Boolean.TRUE.equals(spec.get("notify"))
                && Boolean.TRUE.equals(spec.get("checkWhileRunning"))
                && (os == Target.Os.MACOS || os == Target.Os.WINDOWS);
    }
}
