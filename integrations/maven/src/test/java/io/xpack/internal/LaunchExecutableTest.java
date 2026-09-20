package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class LaunchExecutableTest {

    private static Path withRuntime(Path payload, String executable) throws Exception {
        Files.createDirectories(payload.resolve("runtime/bin"));
        Files.writeString(payload.resolve("runtime/bin/" + executable), "");
        return payload;
    }

    @Test
    void a_bundled_runtime_is_named_by_its_path_inside_the_payload(@TempDir Path payload)
            throws Exception {
        withRuntime(payload, "java");
        String resolved = LaunchExecutable.resolve(
                true, "java", Target.parse("linux-x64"), payload, "runtime");

        // A separator is what tells the launcher to look inside the payload.
        assertEquals("runtime/bin/java", resolved);
        assertTrue(resolved.contains("/"));
    }

    @Test
    void an_unbundled_runtime_is_a_bare_name_the_machine_looks_up(@TempDir Path payload) {
        String resolved = LaunchExecutable.resolve(
                false, "java", Target.parse("linux-x64"), payload, "runtime");

        // No separator, or the launcher would look for a file that is not
        // in the payload.
        assertEquals("java", resolved);
        assertEquals(-1, resolved.indexOf('/'));
    }

    @Test
    void an_unbundled_runtime_on_windows_carries_the_extension(@TempDir Path payload) {
        assertEquals("java.exe", LaunchExecutable.resolve(
                false, "java", Target.parse("windows-x64"), payload, "runtime"));
    }

    @Test
    void a_command_that_is_a_path_is_refused_with_the_reason(@TempDir Path payload) {
        // It would be read as payload-relative, and the package would then be
        // refused for naming a launch executable it does not contain — which
        // is a confusing way to find out about a typo.
        for (String wrong : new String[] {"/usr/bin/java", "bin/java", "C:\\jdk\\java.exe"}) {
            IllegalStateException e = assertThrows(IllegalStateException.class,
                    () -> LaunchExecutable.resolve(
                            false, wrong, Target.parse("linux-x64"), payload, "runtime"));
            assertTrue(e.getMessage().contains("bare name"), e.getMessage());
        }
    }

    @Test
    void an_unbundled_runtime_with_no_command_is_refused(@TempDir Path payload) {
        for (String empty : new String[] {null, "", "   "}) {
            assertThrows(IllegalStateException.class, () -> LaunchExecutable.resolve(
                    false, empty, Target.parse("linux-x64"), payload, "runtime"));
        }
    }

    @Test
    void a_bundled_runtime_that_was_never_linked_says_what_to_do(@TempDir Path payload) {
        IllegalStateException e = assertThrows(IllegalStateException.class,
                () -> LaunchExecutable.resolve(
                        true, "java", Target.parse("linux-x64"), payload, "runtime"));

        assertTrue(e.getMessage().contains("xpack:runtime"), e.getMessage());
        assertTrue(e.getMessage().contains("bundled"), e.getMessage());
    }
}
