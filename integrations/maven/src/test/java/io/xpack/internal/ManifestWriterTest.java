package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.xpack.config.DesktopSpec;
import io.xpack.config.HealthSpec;
import io.xpack.config.PromptSpec;
import io.xpack.config.UpdateSpec;
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
    void writes_the_polling_and_prompt_settings_when_they_are_asked_for() {
        UpdateSpec update = new UpdateSpec();
        update.setChannel("stable");
        update.setCheckWhileRunning(true);
        update.setCheckIntervalMinutes(180);
        update.setNotify(true);
        update.setSeverity("recommended");
        PromptSpec prompt = new PromptSpec();
        prompt.setTitle("A new version is ready");
        prompt.setMessage("Restart when convenient.");
        update.setPrompt(prompt);

        Map<String, Object> manifest =
                Json.parseObject(minimal().update("https://example.com/demo/linux-x64", update).toJson());

        @SuppressWarnings("unchecked")
        Map<String, Object> written = (Map<String, Object>) manifest.get("update");
        assertEquals(Boolean.TRUE, written.get("checkWhileRunning"));
        assertEquals(180L, written.get("checkIntervalMinutes"));
        assertEquals(Boolean.TRUE, written.get("notify"));
        assertEquals("recommended", written.get("severity"));

        @SuppressWarnings("unchecked")
        Map<String, Object> writtenPrompt = (Map<String, Object>) written.get("prompt");
        assertEquals("A new version is ready", writtenPrompt.get("title"));
        assertEquals("Restart when convenient.", writtenPrompt.get("message"));
    }

    @Test
    void writes_nothing_about_polling_when_nothing_was_asked_for() {
        // A POM that says nothing must produce the manifest it always did.
        // Every one of these fields is absent-means-default on the other side,
        // and writing them as nulls would be a change of meaning.
        UpdateSpec update = new UpdateSpec();
        update.setChannel("stable");

        String json = minimal().update("https://example.com/demo/linux-x64", update).toJson();

        assertFalse(json.contains("checkWhileRunning"), json);
        assertFalse(json.contains("checkIntervalMinutes"), json);
        assertFalse(json.contains("notify"), json);
        assertFalse(json.contains("severity"), json);
        assertFalse(json.contains("prompt"), json);
    }

    @Test
    void a_prompt_with_nothing_in_it_is_left_out_rather_than_written_empty() {
        UpdateSpec update = new UpdateSpec();
        update.setChannel("stable");
        update.setPrompt(new PromptSpec());

        String json = minimal().update("https://example.com/demo/linux-x64", update).toJson();

        assertFalse(json.contains("prompt"), json);
    }

    @Test
    void an_interval_alone_does_not_turn_on_checking_while_running() {
        // The two are written independently, because they answer different
        // questions on the other side: one says how often the server may be
        // asked at all, the other whether anything looks while the
        // application is open. A POM that sets only the number must not be
        // read as having asked for both.
        UpdateSpec update = new UpdateSpec();
        update.setCheckIntervalMinutes(45);

        String json = minimal().update("https://example.com/demo/linux-x64", update).toJson();

        assertTrue(json.contains("checkIntervalMinutes"), json);
        assertFalse(json.contains("checkWhileRunning"), json);
    }

    @Test
    void checking_while_running_can_be_asked_for_without_naming_a_rate() {
        // And the other way round: the switch alone is a complete answer,
        // because the interval has a default. Under the older arrangement,
        // where the number was also the switch, this POM checked nothing.
        UpdateSpec update = new UpdateSpec();
        update.setCheckWhileRunning(true);

        String json = minimal().update("https://example.com/demo/linux-x64", update).toJson();

        assertTrue(json.contains("\"checkWhileRunning\": true"), json);
        assertFalse(json.contains("checkIntervalMinutes"), json);
    }

    @Test
    void the_interval_is_a_number_rather_than_a_string() {
        // It is read as one on the other side, and a quoted number there is a
        // parse failure at install time rather than at build time.
        UpdateSpec update = new UpdateSpec();
        update.setCheckIntervalMinutes(45);

        String json = minimal().update("https://example.com/demo/linux-x64", update).toJson();

        assertTrue(json.contains("\"checkIntervalMinutes\": 45"), json);
    }

    @Test
    void writes_the_sections_that_were_configured() {
        HealthSpec health = new HealthSpec();
        health.setRequireStartupReport(true);
        health.setStartupTimeoutSeconds(30);

        DesktopSpec desktop = new DesktopSpec();
        desktop.setShortcut(true);
        desktop.setCategories(List.of("Utility"));

        UpdateSpec update = new UpdateSpec();
        update.setChannel("stable");
        update.setMandatory(false);

        Map<String, Object> manifest = Json.parseObject(minimal()
                .update("https://example.com/demo/linux-x64", update)
                .health(health)
                .desktop(desktop)
                .toJson());

        @SuppressWarnings("unchecked")
        Map<String, Object> written = (Map<String, Object>) manifest.get("update");
        assertEquals("https://example.com/demo/linux-x64", written.get("url"));
        assertEquals("stable", written.get("channel"));
        assertEquals(Boolean.FALSE, written.get("mandatory"));

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
