package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.List;
import org.junit.jupiter.api.Test;

class XPackCliTest {

    /**
     * Checked against {@code xpack <cmd> --help} for every subcommand. Passing
     * --json to one that does not take it is a hard argument error, so this
     * list decides which of two methods a caller has to use.
     */
    @Test
    void knows_which_subcommands_can_be_asked_for_json() {
        for (String subcommand : new String[] {"pack", "delta", "index", "installer", "inspect",
                "list"}) {
            assertTrue(XPackCli.isMachineReadable(subcommand), subcommand);
        }
        for (String subcommand : new String[] {"verify", "update", "install", "activate",
                "rollback", "prune", "uninstall", "recover", "run", "keygen"}) {
            assertFalse(XPackCli.isMachineReadable(subcommand), subcommand);
        }
    }

    /**
     * The plugin reads its own packages only for their platform and version,
     * so the notice every unverified read prints is dropped. Nothing else is.
     */
    @Test
    void inspecting_its_own_packages_does_not_warn_about_verification() {
        String stderr = "warning: this manifest has NOT been verified; use `xpack verify` to check it\n"
                + " WARN something the reader should see\n"
                + "\n";
        assertEquals(List.of(" WARN something the reader should see"),
                XPackCli.worthShowing(stderr));
    }

    @Test
    void the_password_variable_is_passed_only_to_commands_that_read_a_package() {
        XPackCli cli = new XPackCli(Path.of("xpack"),
                new org.apache.maven.plugin.logging.SystemStreamLog(), 1, "RELEASE_PASSWORD");
        org.junit.jupiter.api.Assertions.assertEquals(
                List.of("xpack", "pack", "payload", "--password-env", "RELEASE_PASSWORD"),
                cli.commandFor("pack", List.of("payload")));
        org.junit.jupiter.api.Assertions.assertEquals(
                List.of("xpack", "list"), cli.commandFor("list", List.of()));
    }

    /**
     * A sealed package is tested from its opened copy, so `hooks test` needs
     * the password; `hooks check` reads no package and refuses the flag.
     */
    @Test
    void the_password_goes_to_hooks_test_and_not_hooks_check() {
        XPackCli cli = new XPackCli(Path.of("xpack"), null, 1, "RELEASE_PASSWORD");
        assertTrue(cli.commandFor("hooks", List.of("test", "demo.xpkg")).contains("--password-env"));
        assertFalse(cli.commandFor("hooks", List.of("check", "payload")).contains("--password-env"));
    }
}
