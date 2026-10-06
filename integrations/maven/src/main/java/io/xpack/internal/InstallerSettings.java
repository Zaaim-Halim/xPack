package io.xpack.internal;

import io.xpack.config.InstallerUiSpec;
import java.nio.file.Path;
import java.util.List;

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
                .put("launchOnFinish", spec.getLaunchOnFinish())
                .put("allUsers", spec.getAllUsers() == null || spec.getAllUsers().isBlank()
                        ? null
                        : spec.getAllUsers().trim())
                .put("allUsersDefault", spec.getAllUsersDefault());
        return document.toString();
    }

    /**
     * The command-line arguments that point an installer for {@code target}
     * at that platform's xPack release.
     *
     * <p>Only the folder: which stub and runtime programs go in is the command
     * line's to decide from it, by the rules it applies to its own. None for
     * the build machine's own platform without a folder set, where the command
     * line uses the programs beside itself.
     *
     * @param configured the folder set for {@code target}, or null
     * @throws IllegalArgumentException for another platform with no folder,
     *     saying how to set one
     */
    public static List<String> targetBinaryArguments(Target target, Target host, String configured) {
        if (configured == null || configured.isBlank()) {
            if (!target.equals(host)) {
                throw new IllegalArgumentException(
                        "no xPack release set for " + target.id() + ". An installer is that "
                                + "platform's own installer program with the package attached, so "
                                + "building one here needs xPack's " + target.id() + " release, unpacked:\n"
                                + "  <targetBinaries><" + target.id() + ">/path/to/xpack-<version>-"
                                + target.id() + "</" + target.id() + "></targetBinaries>");
            }
            return List.of();
        }
        return List.of("--target-binaries", Path.of(configured).toAbsolutePath().toString());
    }

    /**
     * The command-line arguments that sign an installer for {@code target}.
     *
     * <p>Only a Windows installer is signed this way, so in a build that makes
     * several platforms the command applies to the Windows ones and the others
     * are built as before, not refused. None when no command is configured.
     */
    public static List<String> signArguments(Target target, String signCommand) {
        if (signCommand == null || signCommand.isBlank() || target.os() != Target.Os.WINDOWS) {
            return List.of();
        }
        return List.of("--sign-command", signCommand);
    }
}
