package io.xpack.config;

import java.util.ArrayList;
import java.util.List;

/**
 * Scripts the application's packages run at moments of an installation's
 * life, and what they may do.
 *
 * <p>One list of {@code <hook>} per operation: {@code install},
 * {@code update}, {@code rollback} and {@code uninstall}. {@code permissions}
 * holds a {@code user} and a {@code machine} block, for an installation for
 * one user and one for everyone. Passed to {@code xpack pack} as it stands,
 * each script prefixed with {@code xpack/hooks/}, where the plugin puts the
 * hooks directory; every rule is the command line's to check.
 */
public class HooksSpec {

    private List<HookSpec> install = new ArrayList<>();

    private List<HookSpec> update = new ArrayList<>();

    private List<HookSpec> rollback = new ArrayList<>();

    private List<HookSpec> uninstall = new ArrayList<>();

    private PermissionsSpec permissions = new PermissionsSpec();

    public List<HookSpec> getInstall() {
        return install;
    }

    public void setInstall(List<HookSpec> install) {
        this.install = install == null ? new ArrayList<>() : install;
    }

    public List<HookSpec> getUpdate() {
        return update;
    }

    public void setUpdate(List<HookSpec> update) {
        this.update = update == null ? new ArrayList<>() : update;
    }

    public List<HookSpec> getRollback() {
        return rollback;
    }

    public void setRollback(List<HookSpec> rollback) {
        this.rollback = rollback == null ? new ArrayList<>() : rollback;
    }

    public List<HookSpec> getUninstall() {
        return uninstall;
    }

    public void setUninstall(List<HookSpec> uninstall) {
        this.uninstall = uninstall == null ? new ArrayList<>() : uninstall;
    }

    public PermissionsSpec getPermissions() {
        return permissions;
    }

    /** {@code <permissions><user>…</user><machine>…</machine></permissions>}. */
    public void setPermissions(PermissionsSpec permissions) {
        this.permissions = permissions == null ? new PermissionsSpec() : permissions;
    }

    public boolean isEmpty() {
        return install.isEmpty() && update.isEmpty() && rollback.isEmpty()
                && uninstall.isEmpty() && permissions.getUser().isEmpty()
                && permissions.getMachine().isEmpty();
    }

    /** {@code <permissions>}: what the hooks may do, by scope. */
    public static class PermissionsSpec {

        private ScopePermissionsSpec user = new ScopePermissionsSpec();

        private ScopePermissionsSpec machine = new ScopePermissionsSpec();

        public ScopePermissionsSpec getUser() {
            return user;
        }

        public void setUser(ScopePermissionsSpec user) {
            this.user = user == null ? new ScopePermissionsSpec() : user;
        }

        public ScopePermissionsSpec getMachine() {
            return machine;
        }

        public void setMachine(ScopePermissionsSpec machine) {
            this.machine = machine == null ? new ScopePermissionsSpec() : machine;
        }
    }
}
