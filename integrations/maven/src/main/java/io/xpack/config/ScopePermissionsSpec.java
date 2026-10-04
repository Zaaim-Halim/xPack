package io.xpack.config;

import java.util.ArrayList;
import java.util.List;

/**
 * What hooks may do in one kind of installation: the programs they may run,
 * each {@code <program>} a file name or an absolute path, and the places they
 * may write, each {@code <place>} starting with a placeholder such as
 * {@code {home}} or, for everyone, an absolute path.
 */
public class ScopePermissionsSpec {

    private List<String> exec = new ArrayList<>();

    private List<String> write = new ArrayList<>();

    public List<String> getExec() {
        return exec;
    }

    public void setExec(List<String> exec) {
        this.exec = exec == null ? new ArrayList<>() : exec;
    }

    public List<String> getWrite() {
        return write;
    }

    public void setWrite(List<String> write) {
        this.write = write == null ? new ArrayList<>() : write;
    }

    public boolean isEmpty() {
        return exec.isEmpty() && write.isEmpty();
    }
}
