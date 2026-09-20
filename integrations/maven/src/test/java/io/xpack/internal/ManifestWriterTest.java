package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.xpack.config.DesktopSpec;
import io.xpack.config.HealthSpec;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class ManifestWriterTest {

    private static ManifestWriter minimal() {
        return new ManifestWriter()
                .id("com.example.demo")
                .name("Demo")
                .version("1.0.0")
                .target(new Target(Target.Os.LINUX, Target.Arch.X64))
                .executable("runtime/bin/java")
                .arguments(List.of("-cp", "application/*", "com.example.Main"));
    }

    @Test
    void writes_the_fields_the_command_line_requires() {
        Map<String, Object> manifest = Json.parseObject(minimal().toJson());

        @SuppressWarnings("unchecked")
        Map<String, Object> application = (Map<String, Object>) manifest.get("application");
        assertEquals("com.example.demo", application.get("id"));
        assertEquals("1.0.0", application.get("version"));

        @SuppressWarnings("unchecked")
        Map<String, Object> platform = (Map<String, Object>) manifest.get("platform");
        assertEquals("linux", platform.get("os"));
        assertEquals("x64", platform.get("arch"));

        @SuppressWarnings("unchecked")
        Map<String, Object> launch = (Map<String, Object>) manifest.get("launch");
        assertEquals("runtime/bin/java", launch.get("executable"));
        assertEquals(List.of("-cp", "application/*", "com.example.Main"), launch.get("arguments"));
    }

    /**
     * The version directory is the default working directory and "." is
     * refused outright, so writing the value that looks like the default
     * produces a manifest that will not load.
     */
    @Test
    void never_writes_a_working_directory() {
        String json = minimal().toJson();
        assertFalse(json.contains("workingDirectory"), json);
    }

    @Test
    void omits_sections_that_were_never_configured() {
        String json = minimal().toJson();
        assertFalse(json.contains("\"update\""), json);
        assertFalse(json.contains("\"health\""), json);
        assertFalse(json.contains("\"desktop\""), json);
    }

    @Test
    void writes_the_sections_that_were_configured() {
        HealthSpec health = new HealthSpec();
        health.setRequireStartupReport(true);
        health.setStartupTimeoutSeconds(30);

        DesktopSpec desktop = new DesktopSpec();
        desktop.setShortcut(true);
        desktop.setCategories(List.of("Utility"));

        Map<String, Object> manifest = Json.parseObject(minimal()
                .update("https://example.com/demo/linux-x64", "stable", false)
                .health(health)
                .desktop(desktop)
                .toJson());

        @SuppressWarnings("unchecked")
        Map<String, Object> update = (Map<String, Object>) manifest.get("update");
        assertEquals("https://example.com/demo/linux-x64", update.get("url"));
        assertEquals("stable", update.get("channel"));
        assertEquals(Boolean.FALSE, update.get("mandatory"));

        @SuppressWarnings("unchecked")
        Map<String, Object> health2 = (Map<String, Object>) manifest.get("health");
        assertEquals(30L, health2.get("startupTimeoutSeconds"));
        assertEquals(Boolean.TRUE, health2.get("requireStartupReport"));

        @SuppressWarnings("unchecked")
        Map<String, Object> desktop2 = (Map<String, Object>) manifest.get("desktop");
        assertEquals(List.of("Utility"), desktop2.get("categories"));
    }

    @Test
    void never_writes_the_payload_inventory() {
        // It is computed from the files on disk. A manifest that disagreed
        // with its own archive would fail verification on every client.
        assertFalse(minimal().toJson().contains("payload"), "the writer must not invent a payload");
    }

    @Test
    void a_name_with_json_syntax_in_it_is_escaped() {
        String json = minimal().name("Tom \"and\" Jerry\\").toJson();
        assertEquals("Tom \"and\" Jerry\\",
                ((Map<?, ?>) Json.parseObject(json).get("application")).get("name"));
    }

    @Test
    void a_three_part_version_passes_through() {
        assertEquals("1.2.3", ManifestWriter.normaliseVersion("1.2.3"));
    }

    @Test
    void a_snapshot_stays_a_prerelease_so_it_sorts_below_the_release() {
        assertEquals("1.2.0-SNAPSHOT", ManifestWriter.normaliseVersion("1.2.0-SNAPSHOT"));
    }

    @Test
    void a_short_maven_version_gains_the_component_semver_requires() {
        assertEquals("1.2.0", ManifestWriter.normaliseVersion("1.2"));
        assertEquals("1.0.0", ManifestWriter.normaliseVersion("1"));
        assertEquals("1.2.0-SNAPSHOT", ManifestWriter.normaliseVersion("1.2-SNAPSHOT"));
    }

    @Test
    void a_version_that_cannot_be_a_semantic_version_is_refused_with_advice() {
        IllegalArgumentException e = assertThrows(IllegalArgumentException.class,
                () -> ManifestWriter.normaliseVersion("RELEASE-2024"));
        assertTrue(e.getMessage().contains("xpack.version"), e.getMessage());
    }

    @Test
    void an_incomplete_manifest_is_refused_before_anything_is_written() {
        assertThrows(IllegalStateException.class, () -> new ManifestWriter().toJson());
    }
}
