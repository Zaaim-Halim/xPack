package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

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
}
