package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;

import java.util.List;
import org.junit.jupiter.api.Test;

class LaunchArgumentsTest {

    private static final List<String> JVM = List.of("-Xmx256m");
    private static final List<String> APP = List.of("--gui");

    @Test
    void the_class_path_is_relative_when_the_application_starts_in_its_own_directory() {
        assertEquals(
                List.of("-Xmx256m", "-cp", "application/*", "com.example.Main", "--gui"),
                LaunchArguments.of(JVM, "com.example.Main", "demo.jar", APP, false));
    }

    /**
     * Started where the user is, a relative class path would be looked for in
     * the user's directory and the application would not start at all.
     */
    @Test
    void the_class_path_names_the_version_directory_when_the_working_directory_is_kept() {
        assertEquals(
                List.of("-Xmx256m", "-cp", "{versionDir}/application/*", "com.example.Main", "--gui"),
                LaunchArguments.of(JVM, "com.example.Main", "demo.jar", APP, true));
    }

    @Test
    void a_jar_started_by_its_own_main_class_is_named_the_same_way() {
        assertEquals(
                List.of("-jar", "application/demo.jar"),
                LaunchArguments.of(List.of(), null, "demo.jar", List.of(), false));
        assertEquals(
                List.of("-jar", "{versionDir}/application/demo.jar"),
                LaunchArguments.of(List.of(), " ", "demo.jar", List.of(), true));
    }

    /**
     * The placeholder makes a package format 2, which xPack 0.1.0 cannot
     * update to, so an application that does not need it never gets it.
     */
    @Test
    void nothing_names_the_version_directory_unless_the_working_directory_is_kept() {
        List<String> arguments = LaunchArguments.of(JVM, "com.example.Main", "demo.jar", APP, false);
        assertFalse(String.join(" ", arguments).contains(LaunchArguments.VERSION_DIR));
    }

    @Test
    void the_users_own_arguments_are_passed_as_written() {
        List<String> jvm = List.of("-Dlog=logs/app.log");
        List<String> app = List.of("input.txt");
        List<String> arguments = LaunchArguments.of(jvm, "com.example.Main", "demo.jar", app, true);
        assertEquals("-Dlog=logs/app.log", arguments.get(0));
        assertEquals("input.txt", arguments.get(arguments.size() - 1));
    }
}
