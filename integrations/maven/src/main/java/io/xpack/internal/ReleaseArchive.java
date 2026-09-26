package io.xpack.internal;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.apache.maven.plugin.logging.Log;
import org.eclipse.aether.RepositorySystem;
import org.eclipse.aether.RepositorySystemSession;
import org.eclipse.aether.artifact.Artifact;
import org.eclipse.aether.artifact.DefaultArtifact;
import org.eclipse.aether.repository.RemoteRepository;
import org.eclipse.aether.resolution.ArtifactRequest;
import org.eclipse.aether.resolution.ArtifactResolutionException;
import org.eclipse.aether.resolution.ArtifactResult;
import org.eclipse.aether.resolution.VersionRangeRequest;
import org.eclipse.aether.resolution.VersionRangeResolutionException;
import org.eclipse.aether.resolution.VersionRangeResult;
import org.eclipse.aether.transfer.MetadataNotFoundException;
import org.eclipse.aether.version.Version;

/**
 * The releases already published, as the repository knows them.
 *
 * <p>A differential update is built against a package a user already has,
 * which means a build needs the artefact of an earlier release. Keeping those
 * by hand is the kind of arrangement that works until the one time it does
 * not, so they are resolved from the repository the release was deployed to —
 * the same place the rest of the world gets them.
 *
 * <p>Nothing here decides whether a failure matters; it reports what
 * happened and lets the caller decide. The distinction that makes that
 * possible is between "there is no earlier release" — true of every first
 * release, and not a problem — and "there is one but it could not be had",
 * which means a release is about to go out without the delta its users
 * should have received.
 */
public final class ReleaseArchive {

    /** The artifact type packages are published under. */
    public static final String PACKAGE_TYPE = "xpkg";

    private final RepositorySystem repositories;
    private final RepositorySystemSession session;
    private final List<RemoteRepository> remotes;
    private final Log log;

    public ReleaseArchive(
            RepositorySystem repositories,
            RepositorySystemSession session,
            List<RemoteRepository> remotes,
            Log log) {
        this.repositories = repositories;
        this.session = session;
        this.remotes = remotes;
        this.log = log;
    }

    /**
     * What the repository said, and whether it could be asked at all.
     *
     * <p>An empty list with {@code known} true means there is genuinely no
     * earlier release. An empty list with {@code known} false means the
     * question could not be put — a different situation entirely, and one a
     * caller may well want to fail on.
     */
    public record Releases(List<String> versions, boolean known) {

        static Releases unknown() {
            return new Releases(List.of(), false);
        }
    }

    /**
     * The released versions of one platform's package, oldest first.
     *
     * <p>Snapshots are left out. They are not releases, nobody has one
     * installed, and a delta from a version that was overwritten in place
     * would be built against bytes that no longer exist.
     */
    public Releases releasedVersions(String groupId, String artifactId, String classifier,
            String below) {
        // An unbounded range, because the question is "what exists", and the
        // filtering below is clearer than encoding it in range syntax.
        Artifact query =
                new DefaultArtifact(groupId, artifactId, classifier, PACKAGE_TYPE, "[0,)");
        VersionRangeRequest request = new VersionRangeRequest(query, remotes, null);

        VersionRangeResult result;
        try {
            result = repositories.resolveVersionRange(session, request);
        } catch (VersionRangeResolutionException e) {
            log.warn("xpack: could not ask the repository which versions exist: " + e.getMessage());
            return Releases.unknown();
        }
        Exception failure = unanswered(result);
        if (failure != null) {
            log.warn("xpack: could not ask the repository which versions exist: " + failure.getMessage());
            return Releases.unknown();
        }
        return released(result, below);
    }

    /**
     * Why a repository could not say which versions exist, or null when every
     * repository answered.
     *
     * <p>The resolver does not throw when a repository refuses (a 401 from a
     * registry that wants credentials) or cannot be reached: it records the
     * failure and answers with what the others said, which may be nothing.
     * Read at face value, that is the answer of a first release, and a
     * release would go out with no delta for anyone. A repository that
     * simply has no listing for the artefact is different: that is what a
     * first release looks like, and it is recorded as not found.
     */
    static Exception unanswered(VersionRangeResult result) {
        for (Exception exception : result.getExceptions()) {
            if (!(exception instanceof MetadataNotFoundException)) {
                return exception;
            }
        }
        return null;
    }

    /** The released versions in a complete answer, oldest first, earlier than {@code below}. */
    static Releases released(VersionRangeResult result, String below) {
        List<String> found = new ArrayList<>();
        for (Version version : result.getVersions()) {
            String text = version.toString();
            if (text.endsWith("-SNAPSHOT")) {
                continue;
            }
            // A delta only ever moves forward. One built from the version
            // being released, or from a later one, describes a change nobody
            // can apply.
            if (below != null && compare(text, below) >= 0) {
                continue;
            }
            found.add(text);
        }
        found.sort(ReleaseArchive::compare);
        return new Releases(found, true);
    }

    /**
     * Downloads one released package, or reports that it could not be had.
     *
     * @return the file, or null when the artefact is not in the repository
     */
    public Path resolve(String groupId, String artifactId, String classifier, String version) {
        Artifact wanted =
                new DefaultArtifact(groupId, artifactId, classifier, PACKAGE_TYPE, version);
        try {
            ArtifactResult result =
                    repositories.resolveArtifact(session, new ArtifactRequest(wanted, remotes, null));
            return result.getArtifact().getFile().toPath();
        } catch (ArtifactResolutionException e) {
            log.warn("xpack: no published package for " + version + " (" + classifier
                    + "), so no delta from it: " + e.getMessage());
            return null;
        }
    }

    /**
     * The newest {@code count} released versions, oldest first.
     *
     * <p>Oldest first so the deltas are built and reported in the order a user
     * would travel through them.
     */
    public Releases latestReleases(String groupId, String artifactId, String classifier,
            String below, int count) {
        Releases all = releasedVersions(groupId, artifactId, classifier, below);
        if (!all.known() || all.versions().size() <= count) {
            return all;
        }
        List<String> versions = all.versions();
        return new Releases(
                new ArrayList<>(versions.subList(versions.size() - count, versions.size())), true);
    }

    /**
     * Resolves several versions, skipping those that cannot be had.
     *
     * <p>Keyed by version so a caller can say which release each file is, and
     * ordered, because the order deltas are reported in is the order they are
     * published in.
     */
    public Map<String, Path> resolveAll(
            String groupId, String artifactId, String classifier, List<String> versions) {
        Map<String, Path> found = new LinkedHashMap<>();
        for (String version : versions) {
            Path file = resolve(groupId, artifactId, classifier, version);
            if (file != null) {
                found.put(version, file);
            }
        }
        return found;
    }

    /** The versions asked for that could not be fetched. */
    public static List<String> missing(List<String> wanted, Map<String, Path> found) {
        List<String> absent = new ArrayList<>();
        for (String version : wanted) {
            if (!found.containsKey(version)) {
                absent.add(version);
            }
        }
        return absent;
    }

    /**
     * Orders two versions the way a release sequence runs.
     *
     * <p>Numeric component by component, so 1.10.0 comes after 1.9.0 rather
     * than before it, which is what comparing the strings would say.
     */
    static int compare(String left, String right) {
        String[] a = left.split("[.+-]");
        String[] b = right.split("[.+-]");
        for (int i = 0; i < Math.max(a.length, b.length); i++) {
            String x = i < a.length ? a[i] : "0";
            String y = i < b.length ? b[i] : "0";
            int order;
            if (x.matches("\\d+") && y.matches("\\d+")) {
                order = Long.compare(Long.parseLong(x), Long.parseLong(y));
            } else {
                order = x.compareTo(y);
            }
            if (order != 0) {
                return order;
            }
        }
        return 0;
    }

    /** Orders versions oldest first. */
    public static Comparator<String> byVersion() {
        return ReleaseArchive::compare;
    }
}
