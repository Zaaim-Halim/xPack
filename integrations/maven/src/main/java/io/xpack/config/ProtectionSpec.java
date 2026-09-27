package io.xpack.config;

/**
 * Locking the application behind a password.
 *
 * <p>Both locks are off unless turned on. {@code installer} makes the
 * installer ask for the password before it installs anything;
 * {@code packages} also seals every package, update and delta, so one taken
 * from the update server cannot be installed without it, and needs
 * {@code installer}. The password is never written in the POM: it is read
 * from the environment variable {@code passwordEnv} names,
 * {@code XPACK_PASSWORD} when left out, in the environment of the build.
 */
public class ProtectionSpec {

    private Boolean installer;

    private Boolean packages;

    private String passwordEnv;

    public Boolean getInstaller() {
        return installer;
    }

    public void setInstaller(Boolean installer) {
        this.installer = installer;
    }

    public Boolean getPackages() {
        return packages;
    }

    public void setPackages(Boolean packages) {
        this.packages = packages;
    }

    public String getPasswordEnv() {
        return passwordEnv;
    }

    public void setPasswordEnv(String passwordEnv) {
        this.passwordEnv = passwordEnv;
    }

    /** Whether the installer asks for a password. */
    public boolean locksInstaller() {
        return Boolean.TRUE.equals(installer);
    }

    public boolean isEmpty() {
        return installer == null && packages == null
                && (passwordEnv == null || passwordEnv.isBlank());
    }
}
