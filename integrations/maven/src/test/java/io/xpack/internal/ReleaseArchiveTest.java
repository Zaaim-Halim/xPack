package io.xpack.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.eclipse.aether.metadata.DefaultMetadata;
import org.eclipse.aether.metadata.Metadata;
import org.eclipse.aether.repository.RemoteRepository;
import org.eclipse.aether.resolution.VersionRangeRequest;
import org.eclipse.aether.resolution.VersionRangeResult;
import org.eclipse.aether.transfer.MetadataNotFoundException;
import org.eclipse.aether.transfer.MetadataTransferException;
import org.eclipse.aether.util.version.GenericVersionScheme;
import org.junit.jupiter.api.Test;

class ReleaseArchiveTest {

    @Test
    void versions_are_ordered_by_number_rather_than_by_text() {
        // The one that matters: sorting these as strings puts 1.10.0 before
        // 1.9.0, so "the last three releases" would silently pick the wrong
        // three the first time a project reached a double-digit minor.
        List<String> versions =
                new ArrayList<>(List.of("1.9.0", "1.10.0", "1.2.0", "2.0.0", "1.10.1"));
        versions.sort(ReleaseArchive.byVersion());

        assertEquals(List.of("1.2.0", "1.9.0", "1.10.0", "1.10.1", "2.0.0"), versions);
    }

    @Test
    void a_missing_component_counts_as_zero() {
        assertTrue(ReleaseArchive.compare("1.2", "1.2.0") == 0);
        assertTrue(ReleaseArchive.compare("1.2", "1.2.1") < 0);
    }

    @Test
    void a_prerelease_orders_against_a_release_without_throwing() {
        // Not a claim about semver precedence — only that a build carrying
        // one does not fall over while deciding what to build deltas from.
        assertTrue(ReleaseArchive.compare("1.0.0-rc1", "1.0.0") != 0);
        assertTrue(ReleaseArchive.compare("1.0.0", "1.0.0") == 0);
    }

    /**
     * The distinction the whole policy rests on. Both are an empty list, and
     * treating them alike would let an unreachable repository ship a release
     * with no deltas while reporting the same thing as a first release.
     */
    @Test
    void no_releases_is_not_the_same_as_not_knowing() {
        ReleaseArchive.Releases none = new ReleaseArchive.Releases(List.of(), true);
        ReleaseArchive.Releases unknown = new ReleaseArchive.Releases(List.of(), false);

        assertTrue(none.versions().isEmpty());
        assertTrue(unknown.versions().isEmpty());
        assertTrue(none.known());
        assertFalse(unknown.known());
    }

    @Test
    void the_versions_that_could_not_be_fetched_are_named() {
        // So the build can say which releases' users would be affected,
        // rather than that something went wrong.
        Map<String, Path> found = Map.of("1.0.0", Path.of("a.xpkg"));
        List<String> wanted = List.of("1.0.0", "1.1.0", "1.2.0");

        assertEquals(List.of("1.1.0", "1.2.0"), ReleaseArchive.missing(wanted, found));
        assertEquals(List.of(), ReleaseArchive.missing(List.of("1.0.0"), found));
    }

    @Test
    void the_same_version_is_never_ordered_before_or_after_itself() {
        for (String version : new String[] {"1.0.0", "0.0.1", "10.20.30"}) {
            assertEquals(0, ReleaseArchive.compare(version, version));
        }
    }

    private static final Metadata LISTING =
            new DefaultMetadata("com.example", "app", "maven-metadata.xml",
                    Metadata.Nature.RELEASE_OR_SNAPSHOT);
    private static final RemoteRepository REGISTRY =
            new RemoteRepository.Builder("github", "default",
                    "https://maven.pkg.github.com/example/app").build();

    private static VersionRangeResult answer(String... versions)
            throws Exception {
        VersionRangeResult result =
                new VersionRangeResult(
                        new VersionRangeRequest());
        GenericVersionScheme scheme =
                new GenericVersionScheme();
        for (String version : versions) {
            result.addVersion(scheme.parseVersion(version));
        }
        return result;
    }

    @Test
    void a_repository_that_refused_is_not_a_first_release() throws Exception {
        // What GitHub Packages answers without credentials. The resolver
        // records it and returns no versions; taken at its word, that is a
        // first release, and the release ships with no delta for anyone.
        VersionRangeResult result = answer();
        result.addException(new MetadataTransferException(LISTING, REGISTRY,
                "status code: 401, reason phrase: Unauthorized (401)"));

        assertTrue(ReleaseArchive.unanswered(result) != null);
    }

    @Test
    void a_refusal_counts_even_when_another_repository_answered() throws Exception {
        // One repository's versions are not all of them: the refusing one may
        // hold the releases users actually have.
        VersionRangeResult result = answer("1.0.0");
        result.addException(new MetadataTransferException(LISTING, REGISTRY,
                "Connection refused"));

        assertTrue(ReleaseArchive.unanswered(result) != null);
    }

    @Test
    void no_listing_anywhere_is_a_first_release() throws Exception {
        // The first release of anything: the repository answers, and has
        // nothing.
        VersionRangeResult result = answer();
        result.addException(new MetadataNotFoundException(LISTING, REGISTRY));

        assertEquals(null, ReleaseArchive.unanswered(result));
        ReleaseArchive.Releases releases = ReleaseArchive.released(result, "1.0.0");
        assertTrue(releases.known());
        assertTrue(releases.versions().isEmpty());
    }

    @Test
    void a_complete_answer_lists_the_earlier_releases_oldest_first() throws Exception {
        VersionRangeResult result =
                answer("1.10.0", "1.2.0", "1.9.0", "2.0.0-SNAPSHOT", "2.0.0");

        assertEquals(null, ReleaseArchive.unanswered(result));
        assertEquals(List.of("1.2.0", "1.9.0", "1.10.0"), ReleaseArchive.released(result, "2.0.0").versions());
    }
}
