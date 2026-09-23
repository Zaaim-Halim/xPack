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
        spec.setLaunchOnFinish(true);

        Map<String, Object> settings = Json.parseObject(InstallerSettings.toJson(spec));

        assertEquals(List.of("welcome", "license", "install", "finish"), settings.get("pages"));
        assertEquals(Map.of("welcome", "Hello {name}."), settings.get("text"));
        assertEquals(false, settings.get("shortcutDefault"));
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
}
