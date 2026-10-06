package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.xpack.config.InstallerUiSpec;
import java.io.File;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class InstallerSettingsTest {

    @Test
    void every_setting_reaches_the_command_line_under_its_own_name() {
        InstallerUiSpec spec = new InstallerUiSpec();
        spec.setPages(List.of("welcome", "license", "install", "finish"));
        spec.setLicense(new File("LICENSE.txt"));
        spec.setText(Map.of("welcome", "Hello {name}."));
        spec.setShortcutDefault(false);
        spec.setPathDefault(false);
        spec.setLaunchOnFinish(true);
        spec.setAllUsers("offer");
        spec.setAllUsersDefault(true);

        Map<String, Object> settings = Json.parseObject(InstallerSettings.toJson(spec));

        assertEquals(List.of("welcome", "license", "install", "finish"), settings.get("pages"));
        assertEquals(Map.of("welcome", "Hello {name}."), settings.get("text"));
        assertEquals(false, settings.get("shortcutDefault"));
        assertEquals(false, settings.get("pathDefault"));
        assertEquals(true, settings.get("launchOnFinish"));
        assertEquals("offer", settings.get("allUsers"));
        assertEquals(true, settings.get("allUsersDefault"));
    }

    @Test
    void who_it_installs_for_is_left_out_unless_set() {
        InstallerUiSpec spec = new InstallerUiSpec();
        spec.setAllUsers("  ");
        assertTrue(spec.isEmpty());
        Map<String, Object> settings = Json.parseObject(InstallerSettings.toJson(spec));
        assertFalse(settings.containsKey("allUsers"));
    }

    @Test
    void the_licence_is_written_as_an_absolute_path() {
        // The settings file sits in the build directory, not beside the POM,
        // so a relative path would be resolved against the wrong directory.
        InstallerUiSpec spec = new InstallerUiSpec();
        spec.setLicense(new File("LICENSE.txt"));

        String license = Json.string(Json.parseObject(InstallerSettings.toJson(spec)), "license");

        assertTrue(new File(license).isAbsolute(), license);
        assertEquals(new File("LICENSE.txt").getAbsolutePath(), license);
    }

    @Test
    void settings_left_out_are_left_out_so_the_command_line_default_applies() {
        InstallerUiSpec spec = new InstallerUiSpec();
        spec.setLaunchOnFinish(true);

        Map<String, Object> settings = Json.parseObject(InstallerSettings.toJson(spec));

        assertEquals(Map.of("launchOnFinish", true), settings);
    }

    @Test
    void an_empty_block_is_no_settings_at_all() {
        assertTrue(new InstallerUiSpec().isEmpty());
        InstallerUiSpec spec = new InstallerUiSpec();
        spec.setShortcutDefault(true);
        assertFalse(spec.isEmpty());
    }

    @Test
    void an_installer_for_another_platform_is_given_that_platforms_release_folder() {
        Target mac = Target.parse("macos-arm64");
        Target linux = Target.parse("linux-x64");
        // Only the folder: the command line chooses the programs from it.
        assertEquals(List.of("--target-binaries", java.nio.file.Path.of("rel/xpack-linux").toAbsolutePath().toString()),
                InstallerSettings.targetBinaryArguments(linux, mac, "rel/xpack-linux"));
    }

    @Test
    void the_build_machines_own_platform_needs_no_folder() {
        Target mac = Target.parse("macos-arm64");
        assertEquals(List.of(), InstallerSettings.targetBinaryArguments(mac, mac, null));
        assertEquals(List.of(), InstallerSettings.targetBinaryArguments(mac, mac, " "));
    }

    @Test
    void another_platform_without_a_folder_says_how_to_set_one() {
        Target mac = Target.parse("macos-arm64");
        Target windows = Target.parse("windows-x64");
        IllegalArgumentException refused = org.junit.jupiter.api.Assertions.assertThrows(
                IllegalArgumentException.class,
                () -> InstallerSettings.targetBinaryArguments(windows, mac, null));
        assertTrue(refused.getMessage().contains("<targetBinaries><windows-x64>"), refused.getMessage());
    }

    @Test
    void a_windows_installer_is_signed_with_the_configured_command() {
        Target windows = new Target(Target.Os.WINDOWS, Target.Arch.X64);
        String command = "signtool sign /f \"C:\\My Certs\\cert.pfx\" {file}";
        // Passed through untouched, as one argument: the command line splits it.
        assertEquals(List.of("--sign-command", command), InstallerSettings.signArguments(windows, command));
    }

    @Test
    void only_windows_installers_are_signed_and_only_when_asked() {
        Target windows = new Target(Target.Os.WINDOWS, Target.Arch.X64);
        assertEquals(List.of(), InstallerSettings.signArguments(windows, null));
        assertEquals(List.of(), InstallerSettings.signArguments(windows, "  "));
        for (Target.Os os : List.of(Target.Os.MACOS, Target.Os.LINUX)) {
            Target target = new Target(os, Target.Arch.ARM64);
            assertEquals(List.of(), InstallerSettings.signArguments(target, "signtool sign {file}"));
        }
    }
}
