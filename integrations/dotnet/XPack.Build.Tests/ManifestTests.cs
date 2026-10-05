using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using Xunit;

namespace XPack.Build.Tests;

public class ManifestTests
{
    private static ProjectFacts Facts(System.Action<ProjectFacts>? change = null)
    {
        var facts = new ProjectFacts
        {
            Id = "com.example.hello",
            AssemblyName = "Hello",
            Version = "1.2.0",
            RuntimeIdentifier = "linux-x64",
            OutputType = "Exe",
        };
        change?.Invoke(facts);
        return facts;
    }

    private static JsonElement Json(ManifestResult result)
    {
        Assert.True(result.Succeeded, string.Join("; ", result.Errors));
        return JsonDocument.Parse(result.Json).RootElement;
    }

    private static string At(JsonElement root, string path) =>
        path.Split('.').Aggregate(root, (e, key) => e.GetProperty(key)).ToString();

    [Fact]
    public void A_project_maps_to_its_application_and_launch()
    {
        var root = Json(Manifest.Build(Facts(f =>
        {
            f.Product = "Hello World";
            f.Description = "Says hello";
            f.Company = "Example Ltd";
        })));
        Assert.Equal("com.example.hello", At(root, "application.id"));
        Assert.Equal("Hello World", At(root, "application.name"));
        Assert.Equal("1.2.0", At(root, "application.version"));
        Assert.Equal("Says hello", At(root, "application.description"));
        Assert.Equal("Example Ltd", At(root, "application.publisher"));
        Assert.Equal("app/Hello", At(root, "launch.executable"));
    }

    [Fact]
    public void The_name_falls_back_to_the_assembly_and_an_override_wins()
    {
        Assert.Equal("Hello", At(Json(Manifest.Build(Facts())), "application.name"));
        var root = Json(Manifest.Build(Facts(f => { f.Product = "Product"; f.Name = "Override"; f.Company = "Co"; f.Publisher = "Pub"; })));
        Assert.Equal("Override", At(root, "application.name"));
        Assert.Equal("Pub", At(root, "application.publisher"));
    }

    [Theory]
    [InlineData("win-x64", "windows-x64", "app/Hello.exe")]
    [InlineData("osx-x64", "macos-x64", "app/Hello")]
    [InlineData("osx-arm64", "macos-arm64", "app/Hello")]
    [InlineData("linux-x64", "linux-x64", "app/Hello")]
    [InlineData("linux-arm64", "linux-arm64", "app/Hello")]
    public void Each_supported_runtime_identifier_is_its_platform(string rid, string platform, string executable)
    {
        var result = Manifest.Build(Facts(f => f.RuntimeIdentifier = rid));
        Assert.Equal(platform, result.Platform);
        Assert.Equal(executable, At(Json(result), "launch.executable"));
    }

    [Theory]
    [InlineData("win-x86")]
    [InlineData("win-arm64")]
    [InlineData("linux-musl-x64")]
    [InlineData("osx")]
    public void A_runtime_identifier_xpack_does_not_build_for_is_refused_with_the_list(string rid)
    {
        var result = Manifest.Build(Facts(f => f.RuntimeIdentifier = rid));
        Assert.False(result.Succeeded);
        var error = Assert.Single(result.Errors);
        Assert.Contains(rid, error);
        Assert.Contains("osx-arm64", error);
    }

    [Fact]
    public void The_executable_is_named_by_its_path_inside_the_package()
    {
        // A bare name would be looked for on the PATH and never found.
        var overridden = Json(Manifest.Build(Facts(f => f.Executable = "tools\\Hello.Cli")));
        Assert.Equal("app/tools/Hello.Cli", At(overridden, "launch.executable"));
    }

    [Fact]
    public void A_publish_without_a_runtime_identifier_is_refused_naming_the_fix()
    {
        var result = Manifest.Build(Facts(f => f.RuntimeIdentifier = ""));
        Assert.Contains(result.Errors, e => e.Contains("dotnet publish -r"));
    }

    [Fact]
    public void A_missing_id_is_refused_with_an_example()
    {
        var result = Manifest.Build(Facts(f => f.Id = " "));
        Assert.Contains(result.Errors, e => e.Contains("<XPackId>"));
    }

    [Fact]
    public void A_four_part_version_is_refused_and_a_prerelease_is_kept()
    {
        Assert.Contains(Manifest.Build(Facts(f => f.Version = "1.2.3.4")).Errors, e => e.Contains("SemVer"));
        Assert.Equal("1.2.0-beta.1", At(Json(Manifest.Build(Facts(f => f.Version = "1.2.0-beta.1"))), "application.version"));
    }

    [Theory]
    [InlineData("linux-x64")]
    [InlineData("osx-arm64")]
    public void A_unix_package_cannot_be_built_on_windows(string rid)
    {
        var result = Manifest.Build(Facts(f => { f.RuntimeIdentifier = rid; f.HostIsWindows = true; }));
        Assert.Contains(result.Errors, e => e.Contains("cannot be built on Windows"));
    }

    [Fact]
    public void A_windows_package_can_be_built_on_windows()
    {
        Assert.True(Manifest.Build(Facts(f => { f.RuntimeIdentifier = "win-x64"; f.HostIsWindows = true; })).Succeeded);
    }

    [Fact]
    public void A_console_tool_gets_a_terminal_and_a_window_does_not()
    {
        Assert.Equal("True", At(Json(Manifest.Build(Facts())), "desktop.terminal"));
        Assert.Equal("False", At(Json(Manifest.Build(Facts(f => f.OutputType = "WinExe"))), "desktop.terminal"));
        Assert.Equal("False", At(Json(Manifest.Build(Facts(f => f.UsesWpf = true))), "desktop.terminal"));
        Assert.Equal("False", At(Json(Manifest.Build(Facts(f => f.UsesWindowsForms = true))), "desktop.terminal"));
    }

    [Fact]
    public void Avalonia_built_as_exe_is_a_window_with_a_warning_unless_said()
    {
        var guessed = Manifest.Build(Facts(f => f.UsesAvaloniaOrMaui = true));
        Assert.Equal("False", At(Json(guessed), "desktop.terminal"));
        Assert.Single(guessed.Warnings);

        var said = Manifest.Build(Facts(f => { f.UsesAvaloniaOrMaui = true; f.Terminal = "true"; }));
        Assert.Equal("True", At(Json(said), "desktop.terminal"));
        Assert.Empty(said.Warnings);
    }

    [Fact]
    public void A_command_keeps_the_working_directory_unless_turned_off()
    {
        var root = Json(Manifest.Build(Facts(f => f.Command = "hello")));
        Assert.Equal("hello", At(root, "command.name"));
        Assert.Equal("True", At(root, "launch.keepWorkingDirectory"));

        var off = Json(Manifest.Build(Facts(f => { f.Command = "hello"; f.KeepWorkingDirectory = "false"; })));
        Assert.False(off.GetProperty("launch").TryGetProperty("keepWorkingDirectory", out _));
    }

    [Fact]
    public void The_update_url_names_the_platform_being_built()
    {
        var root = Json(Manifest.Build(Facts(f =>
        {
            f.RuntimeIdentifier = "osx-arm64";
            f.UpdateUrl = "https://updates.example.com/hello/{platform}";
            f.UpdateChannel = "beta";
        })));
        Assert.Equal("https://updates.example.com/hello/macos-arm64", At(root, "update.url"));
        Assert.Equal("beta", At(root, "update.channel"));
    }

    [Fact]
    public void The_icon_is_named_inside_the_payload_with_its_own_extension()
    {
        var result = Manifest.Build(Facts(f => f.IconSource = "/somewhere/Assets/App.ICNS"));
        Assert.Equal("xpack/icon.icns", result.IconPayloadPath);
        Assert.Equal("xpack/icon.icns", At(Json(result), "desktop.icon"));
    }

    [Fact]
    public void Arguments_and_single_instance_are_passed_through()
    {
        var root = Json(Manifest.Build(Facts(f =>
        {
            f.Arguments = new List<string> { "--mode", "a b" };
            f.SingleInstance = "true";
        })));
        Assert.Equal(new[] { "--mode", "a b" }, root.GetProperty("launch").GetProperty("arguments").EnumerateArray().Select(e => e.GetString()));
        Assert.True(root.GetProperty("instance").GetProperty("single").GetBoolean());
    }

    [Theory]
    [InlineData("He said \"hi\"")]
    [InlineData("back\\slash and C:\\Program Files\\")]
    [InlineData("line\nbreak\ttab\u0001control")]
    [InlineData("Café — 日本語 ✓")]
    [InlineData("\"}, \"evil\": {\"")]
    public void Any_name_survives_as_exactly_itself(string name)
    {
        // The attack: a product name that tries to close the string and add
        // its own fields. It must come back as one string, unchanged.
        var root = Json(Manifest.Build(Facts(f => f.Name = name)));
        Assert.Equal(name, root.GetProperty("application").GetProperty("name").GetString());
        Assert.False(root.TryGetProperty("evil", out _));
        Assert.False(root.GetProperty("application").TryGetProperty("evil", out _));
    }
}
