package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class TargetTest {

    @Test
    void parses_the_spelling_the_command_line_uses() {
        assertEquals("linux-x64", Target.parse("linux-x64").id());
        assertEquals("macos-arm64", Target.parse("macos-arm64").id());
        assertEquals("windows-x64", Target.parse("windows-x64").id());
    }

    @Test
    void parses_the_spellings_a_ci_matrix_is_likely_to_already_have() {
        assertEquals("macos-arm64", Target.parse("darwin-aarch64").id());
        assertEquals("linux-x64", Target.parse("linux-amd64").id());
        assertEquals("windows-x64", Target.parse("WIN32-X86_64").id());
    }

    @Test
    void refuses_something_that_is_not_a_target() {
        assertThrows(IllegalArgumentException.class, () -> Target.parse("linux"));
        assertThrows(IllegalArgumentException.class, () -> Target.parse("plan9-x64"));
        assertThrows(IllegalArgumentException.class, () -> Target.parse("linux-sparc"));
    }

    @Test
    void only_windows_executables_carry_an_extension() {
        assertEquals("java", Target.parse("linux-x64").executableName("java"));
        assertEquals("java", Target.parse("macos-arm64").executableName("java"));
        assertEquals("java.exe", Target.parse("windows-x64").executableName("java"));
    }

    /**
     * A jlink image puts the interpreter in the same place on every platform,
     * including macOS. Choosing the path from the operating system alone would
     * send a macOS build to the bundle layout a jlink image does not have.
     */
    @Test
    void finds_a_jlink_interpreter_on_macos_where_a_linux_one_would_be(@TempDir Path payload)
            throws Exception {
        Files.createDirectories(payload.resolve("runtime/bin"));
        Files.writeString(payload.resolve("runtime/bin/java"), "");

        assertEquals("runtime/bin/java",
                Target.parse("macos-arm64").findBundledJava(payload, "runtime"));
    }

    @Test
    void finds_the_interpreter_inside_a_copied_macos_jdk_bundle(@TempDir Path payload)
            throws Exception {
        Files.createDirectories(payload.resolve("runtime/Contents/Home/bin"));
        Files.writeString(payload.resolve("runtime/Contents/Home/bin/java"), "");

        assertEquals("runtime/Contents/Home/bin/java",
                Target.parse("macos-arm64").findBundledJava(payload, "runtime"));
    }

    @Test
    void reports_no_interpreter_rather_than_guessing_one(@TempDir Path payload) {
        assertNull(Target.parse("linux-x64").findBundledJava(payload, "runtime"));
    }

    @Test
    void the_host_is_one_of_the_platforms_xpack_supports() {
        assertNotNull(Target.host().id());
    }
}
