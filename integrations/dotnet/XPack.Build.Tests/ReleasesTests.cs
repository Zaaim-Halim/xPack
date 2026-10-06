using System.Linq;
using Xunit;

namespace XPack.Build.Tests;

public class ReleasesTests
{
    [Fact]
    public void Versions_are_ordered_as_semver_orders_them()
    {
        // The order SemVer's own specification gives, lowest first.
        var ordered = new[]
        {
            "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2",
            "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.2.0", "1.10.0", "2.0.0",
        };
        var parsed = ordered.Select(v => SemVer.Parse(v)!).ToList();
        for (var i = 1; i < parsed.Count; i++)
        {
            Assert.True(parsed[i - 1].CompareTo(parsed[i]) < 0, $"{ordered[i - 1]} should come before {ordered[i]}");
            Assert.True(parsed[i].CompareTo(parsed[i - 1]) > 0, $"{ordered[i]} should come after {ordered[i - 1]}");
        }
        Assert.Equal(0, SemVer.Parse("1.0.0+build.5")!.CompareTo(SemVer.Parse("1.0.0")));
    }

    [Theory]
    [InlineData("1.2")]
    [InlineData("1.2.3.4")]
    [InlineData("01.2.3")]
    [InlineData("v1.2.3")]
    [InlineData("")]
    public void What_is_not_semver_is_not_a_candidate(string text)
    {
        Assert.Null(SemVer.Parse(text));
    }

    [Fact]
    public void A_package_is_known_by_its_application_and_platform()
    {
        // As `xpack inspect --json` prints it, with fields named `version`
        // and `id` elsewhere that must not be taken for the application's.
        const string inspected = """
            {
              "formatVersion": 4,
              "application": { "id": "com.example.app", "name": "App \"}\" {", "version": "1.2.0" },
              "platform": { "os": "linux", "arch": "arm64" },
              "launch": { "executable": "app/App", "version": "not-this", "id": "nor-this" }
            }
            """;
        var identity = PackageIdentity.FromInspect(inspected)!;
        Assert.Equal("com.example.app", identity.Id);
        Assert.Equal("1.2.0", identity.Version);
        Assert.Equal("linux-arm64", identity.Platform);
    }

    [Fact]
    public void Output_that_is_not_a_package_description_is_no_identity()
    {
        Assert.Null(PackageIdentity.FromInspect("error: not a package"));
        Assert.Null(PackageIdentity.FromInspect("{\"application\": {\"name\": \"x\"}, \"platform\": {}}"));
    }

    [Fact]
    public void An_object_is_found_whole_past_braces_inside_strings()
    {
        Assert.Equal("{\"a\": \"}{\\\"}\", \"b\": {\"c\": 1}}",
            JsonText.Object("{\"x\": {\"a\": \"}{\\\"}\", \"b\": {\"c\": 1}}, \"y\": 2}", "x"));
        Assert.Null(JsonText.Object("{\"x\": 1}", "x"));
        Assert.Null(JsonText.Object("{\"x\": {\"unclosed\": 1", "x"));
    }
}
