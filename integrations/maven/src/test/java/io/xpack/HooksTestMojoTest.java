package io.xpack;

import static org.junit.jupiter.api.Assertions.assertEquals;

import io.xpack.config.InstallerUiSpec;
import java.io.File;
import java.nio.file.Path;
import java.util.List;
import org.junit.jupiter.api.Test;

class HooksTestMojoTest {

    private static InstallerUiSpec allUsers(String value) {
        InstallerUiSpec spec = new InstallerUiSpec();
        spec.setAllUsers(value);
        return spec;
    }

    /**
     * One user's installation is always tested, as only those update
     * themselves; everyone's too where the installer can install so, as the
     * gate then asks for that report.
     */
    @Test
    void tests_every_scope_the_releases_installations_can_have() {
        assertEquals(List.of(false), HooksTestMojo.scopesFor(null));
        assertEquals(List.of(false), HooksTestMojo.scopesFor(new InstallerUiSpec()));
        assertEquals(List.of(false), HooksTestMojo.scopesFor(allUsers("never")));
        assertEquals(List.of(false, true), HooksTestMojo.scopesFor(allUsers("offer")));
        assertEquals(List.of(false, true), HooksTestMojo.scopesFor(allUsers(" Always ")));
    }

    @Test
    void asks_for_a_plan_of_one_users_installation_by_default() {
        assertEquals(List.of("test", "demo.xpkg"),
                HooksTestMojo.arguments(Path.of("demo.xpkg"), null, false, false, null));
    }

    @Test
    void passes_every_choice_on_to_the_command_line() {
        assertEquals(List.of("test", "demo.xpkg", "--previous", "old.xpkg", "--real",
                        "--answers", "answers.json", "--all-users"),
                HooksTestMojo.arguments(Path.of("demo.xpkg"), Path.of("old.xpkg"), true, true,
                        new File("answers.json")));
    }
}
