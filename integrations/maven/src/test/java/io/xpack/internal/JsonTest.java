package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class JsonTest {

    @Test
    void reads_what_pack_prints() {
        // The shape the plugin actually consumes.
        String output = """
                {
                  "package": "/out/My-App-1.0.0-macos-arm64.xpkg",
                  "application": "com.example.demo",
                  "version": "1.0.0",
                  "platform": "macos-arm64",
                  "files": 99,
                  "size": 32446545,
                  "sha256": "7b3a844a",
                  "signedBy": "610f-e312"
                }
                """;
        Map<String, Object> result = Json.parseObject(output);
        assertEquals("/out/My-App-1.0.0-macos-arm64.xpkg", Json.string(result, "package"));
        assertEquals(99L, Json.number(result, "files"));
        assertEquals(32446545L, Json.number(result, "size"));
    }

    /**
     * The two shapes the command line reports a platform in. Assuming one of
     * them everywhere broke three goals at once, so both are pinned here.
     */
    @Test
    void reads_a_platform_in_either_shape_the_command_line_uses() {
        // `pack --json` reports a string.
        assertEquals("macos-arm64", Json.platform(Json.parseObject(
                "{\"platform\": \"macos-arm64\"}")));

        // `inspect --json` reports the manifest's own object.
        assertEquals("macos-arm64", Json.platform(Json.parseObject(
                "{\"platform\": {\"os\": \"macos\", \"arch\": \"arm64\"}}")));
    }

    @Test
    void a_platform_that_is_neither_shape_is_an_error_rather_than_a_guess() {
        Map<String, Object> empty = Json.parseObject("{}");
        assertThrows(IllegalArgumentException.class, () -> Json.platform(empty));
    }

    @Test
    void reads_the_paths_list_reports() {
        // The shape `list --json` prints, which is how an integration finds
        // executables named after the application rather than after xPack.
        String output = """
                {
                  "application": "com.example.demo",
                  "root": "/r/com.example.demo",
                  "launcher": "/r/com.example.demo/My App",
                  "consoleLauncher": "/r/com.example.demo/My App",
                  "updater": "/r/com.example.demo/My App Updater",
                  "uninstaller": "/r/com.example.demo/Uninstall My App",
                  "versions": [ { "version": "1.0.0", "active": true } ]
                }
                """;
        Map<String, Object> listing = Json.parseObject(output);
        assertEquals("/r/com.example.demo/My App", Json.string(listing, "launcher"));
        assertEquals(1, ((List<?>) listing.get("versions")).size());
    }

    @Test
    void a_null_path_in_a_listing_is_read_as_absent_rather_than_as_a_path() {
        // Nothing is installed, so there is nothing to run. A caller must not
        // be handed the string "null" to execute.
        Map<String, Object> listing = Json.parseObject("{\"launcher\": null}");
        assertNull(listing.get("launcher"));
    }

    @Test
    void reads_the_array_index_prints() {
        String output = """
                [ { "path": "/site/macos-arm64/stable.json",
                    "platform": "macos-arm64",
                    "package": "My-App-1.0.0-macos-arm64.xpkg",
                    "url": "https://example.com/demo/macos-arm64/stable.json" } ]
                """;
        List<?> entries = (List<?>) Json.parse(output);
        assertEquals(1, entries.size());
        assertEquals("macos-arm64", ((Map<?, ?>) entries.get(0)).get("platform"));
    }

    @Test
    void round_trips_a_string_that_would_otherwise_break_the_document() {
        String awkward = "quote \" backslash \\ newline \n tab \t control \u0001";
        String json = new Json.Obj().put("value", awkward).toString();
        assertEquals(awkward, Json.parseObject(json).get("value"));
    }

    @Test
    void omits_a_null_rather_than_writing_one() {
        String json = new Json.Obj().put("kept", "yes").put("dropped", null).toString();
        assertTrue(json.contains("kept"), json);
        assertEquals(1, Json.parseObject(json).size());
    }

    @Test
    void omits_empty_collections() {
        String json = new Json.Obj()
                .put("kept", "yes")
                .putIfAny("list", List.<String>of())
                .putIfAny("map", Map.<String, String>of())
                .putIfAny("object", new Json.Obj())
                .toString();
        assertEquals(1, Json.parseObject(json).size());
    }

    @Test
    void refuses_text_that_is_not_json() {
        assertThrows(IllegalArgumentException.class, () -> Json.parse("{"));
        assertThrows(IllegalArgumentException.class, () -> Json.parse("{} trailing"));
        assertThrows(IllegalArgumentException.class, () -> Json.parse(""));
    }

    @Test
    void a_missing_field_is_an_error_rather_than_a_null() {
        Map<String, Object> empty = Json.parseObject("{}");
        assertThrows(IllegalArgumentException.class, () -> Json.string(empty, "package"));
        assertThrows(IllegalArgumentException.class, () -> Json.number(empty, "size"));
    }
}
