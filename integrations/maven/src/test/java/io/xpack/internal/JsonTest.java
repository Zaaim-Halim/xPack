package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
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
