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

        Map<String, Object> settings = Json.parseObject(InstallerSettings.toJson(spec));

        assertEquals(List.of("welcome", "license", "install", "finish"), settings.get("pages"));
        assertEquals(Map.of("welcome", "Hello {name}."), settings.get("text"));
        assertEquals(false, settings.get("shortcutDefault"));
        assertEquals(false, settings.get("pathDefault"));
        assertEquals(true, settings.get("launchOnFinish"));
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
    void a_windows_installer_is_the_windowed_build_unless_scripts_need_the_console() {
        Target windows = new Target(Target.Os.WINDOWS, Target.Arch.X64);
        assertEquals("xpack-installerw", InstallerSettings.stubName(windows, false));
        assertEquals("xpack-installer", InstallerSettings.stubName(windows, true));
    }

    @Test
    void other_targets_have_one_installer_build() {
        for (Target.Os os : List.of(Target.Os.MACOS, Target.Os.LINUX)) {
            Target target = new Target(os, Target.Arch.ARM64);
            assertEquals("xpack-installer", InstallerSettings.stubName(target, false));
            assertEquals("xpack-installer", InstallerSettings.stubName(target, true));
        }
    }

    private static Map<String, Object> manifest(String os, boolean notify, boolean checkWhileRunning) {
        // As `xpack inspect --json` reports it: a flag left at false is not written.
        Map<String, Object> update = new java.util.HashMap<>();
        update.put("channel", "stable");
        if (notify) {
            update.put("notify", true);
        }
        if (checkWhileRunning) {
            update.put("checkWhileRunning", true);
        }
        return Map.of("platform", Map.of("os", os, "arch", "x64"), "update", update);
    }

    @Test
    void the_notice_is_wanted_exactly_when_installing_would_place_it() {
        assertTrue(InstallerSettings.wantsNotice(manifest("macos", true, true)));
        assertTrue(InstallerSettings.wantsNotice(manifest("windows", true, true)));
        // No dialog on Linux.
        assertFalse(InstallerSettings.wantsNotice(manifest("linux", true, true)));
        // Nothing checks while running, so there is nothing to announce.
        assertFalse(InstallerSettings.wantsNotice(manifest("macos", true, false)));
        assertFalse(InstallerSettings.wantsNotice(manifest("windows", false, true)));
        assertFalse(InstallerSettings.wantsNotice(Map.of("platform", "macos-arm64")));
    }

    @Test
    void a_cross_built_installer_carries_the_notice_it_was_asked_for() {
        // Only an installer can place the notice: left out here, every update
        // of the application would be applied without a word.
        Target mac = Target.parse("macos-arm64");
        Target windows = Target.parse("windows-x64");
        assertEquals(List.of("xpack-launcher", "xpack-updater", "xpack-uninstaller", "xpack-notify"),
                InstallerSettings.runtimeBinaries(mac, true));
        assertEquals(List.of("xpack-launcher", "xpack-updater", "xpack-uninstaller"),
                InstallerSettings.runtimeBinaries(mac, false));
        assertEquals(List.of("xpack-launcher", "xpack-updater", "xpack-uninstaller", "xpack-launcherw",
                "xpack-notify"), InstallerSettings.runtimeBinaries(windows, true));
    }
}
