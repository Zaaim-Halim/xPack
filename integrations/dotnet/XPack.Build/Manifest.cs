using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text.RegularExpressions;

namespace XPack.Build;

/// <summary>What a project says about itself, as MSBuild knows it.</summary>
internal sealed class ProjectFacts
{
    public string Id { get; set; } = "";
    public string AssemblyName { get; set; } = "";
    public string Product { get; set; } = "";
    public string Version { get; set; } = "";
    public string Description { get; set; } = "";
    public string Company { get; set; } = "";
    public string RuntimeIdentifier { get; set; } = "";
    public string OutputType { get; set; } = "";
    public bool UsesWpf { get; set; }
    public bool UsesWindowsForms { get; set; }
    public bool UsesAvaloniaOrMaui { get; set; }
    public bool HostIsWindows { get; set; }

    public string Name { get; set; } = "";
    public string Publisher { get; set; } = "";
    public string Executable { get; set; } = "";
    public IReadOnlyList<string> Arguments { get; set; } = Array.Empty<string>();
    public string Command { get; set; } = "";
    public string KeepWorkingDirectory { get; set; } = "";
    public string Terminal { get; set; } = "";
    public string Shortcut { get; set; } = "";
    public string UpdateUrl { get; set; } = "";
    public string UpdateChannel { get; set; } = "";
    public string SingleInstance { get; set; } = "";

    /// <summary>The icon's file on disk, already chosen for the target platform.</summary>
    public string IconSource { get; set; } = "";
}

/// <summary>The <c>xpack.json</c> a project's facts make, or why they make none.</summary>
internal sealed class ManifestResult
{
    public string Json { get; set; } = "";
    public string Platform { get; set; } = "";
    /// <summary>Where the icon goes in the payload, with forward slashes; empty without one.</summary>
    public string IconPayloadPath { get; set; } = "";
    public List<string> Errors { get; } = new List<string>();
    public List<string> Warnings { get; } = new List<string>();
    public bool Succeeded => Errors.Count == 0;
}

/// <summary>Maps a project's MSBuild properties to an <c>xpack.json</c>.</summary>
/// <remarks>
/// Only the mapping lives here. Whether the result is a valid package is the
/// xpack command line's to decide, so nothing it checks is checked twice;
/// what is refused here is only what a .NET developer can fix in .NET terms,
/// said in those terms.
/// </remarks>
internal static class Manifest
{
    /// <summary>Runtime identifiers xPack builds for, and the platform each one is.</summary>
    internal static readonly IReadOnlyDictionary<string, string> Platforms = new Dictionary<string, string>
    {
        ["win-x64"] = "windows-x64",
        ["osx-x64"] = "macos-x64",
        ["osx-arm64"] = "macos-arm64",
        ["linux-x64"] = "linux-x64",
        ["linux-arm64"] = "linux-arm64",
    };

    /// <summary>The folder inside the payload that holds what xPack adds to a publish.</summary>
    internal const string PayloadFolder = "xpack";

    /// <summary>
    /// The folder inside the payload that holds the publish output.
    /// </summary>
    /// <remarks>
    /// A launch executable with no folder in its path is a program xPack looks
    /// for on the PATH, as <c>java</c> or <c>python3</c> would be; one inside the
    /// package must be named by its path in it. Keeping the publish in a folder
    /// of its own makes that path, and keeps it apart from what xPack adds.
    /// </remarks>
    internal const string ApplicationFolder = "app";

    private static readonly Regex FourPartVersion = new Regex(@"^\d+\.\d+\.\d+\.\d+$");

    /// <summary>The xPack platform of the machine this runs on, or empty when xPack has none.</summary>
    internal static string HostPlatform(OSPlatform? os = null, Architecture? architecture = null)
    {
        var arch = (architecture ?? RuntimeInformation.OSArchitecture) switch
        {
            Architecture.X64 => "x64",
            Architecture.Arm64 => "arm64",
            _ => "",
        };
        bool Is(OSPlatform platform) => os.HasValue ? os.Value == platform : RuntimeInformation.IsOSPlatform(platform);
        var system = Is(OSPlatform.Windows) ? "windows"
            : Is(OSPlatform.OSX) ? "macos"
            : Is(OSPlatform.Linux) ? "linux"
            : "";
        return system.Length == 0 || arch.Length == 0 ? "" : system + "-" + arch;
    }

    public static ManifestResult Build(ProjectFacts facts)
    {
        var result = new ManifestResult();

        if (facts.Id.Trim().Length == 0)
        {
            result.Errors.Add(
                "XPackId is not set. Give the application a stable reverse-DNS id, such as "
                + "<XPackId>com.example.myapp</XPackId>; installations recognise their updates by it.");
        }

        if (facts.RuntimeIdentifier.Length == 0)
        {
            result.Errors.Add(
                "this publish has no runtime identifier. xPack packages one platform at a time: "
                + "publish with -r, for example `dotnet publish -r win-x64`.");
        }
        else if (Platforms.TryGetValue(facts.RuntimeIdentifier, out var platform))
        {
            result.Platform = platform;
        }
        else
        {
            result.Errors.Add(
                $"xPack does not build for the runtime identifier {facts.RuntimeIdentifier}. "
                + "It builds for " + string.Join(", ", Platforms.Keys) + ".");
        }

        var windowsTarget = result.Platform.StartsWith("windows", StringComparison.Ordinal);
        if (facts.HostIsWindows && result.Platform.Length > 0 && !windowsTarget)
        {
            result.Errors.Add(
                $"a {result.Platform} package cannot be built on Windows: Windows does not record "
                + "the Unix permission bits that make the application executable, so it would "
                + "install but not start. Build it on macOS or Linux.");
        }

        var version = facts.Version.Trim();
        if (FourPartVersion.IsMatch(version))
        {
            result.Errors.Add(
                $"the version {version} has four parts; xPack versions are SemVer, three parts "
                + "and an optional pre-release (1.2.0, 1.2.0-beta.1). Set <Version> to one.");
        }

        if (!result.Succeeded)
        {
            return result;
        }

        var application = new List<KeyValuePair<string, object>>
        {
            Pair("id", facts.Id.Trim()),
            Pair("name", FirstOf(facts.Name, facts.Product, facts.AssemblyName)),
            Pair("version", version),
        };
        AddIfSet(application, "description", facts.Description);
        AddIfSet(application, "publisher", FirstOf(facts.Publisher, facts.Company));

        var executable = ApplicationFolder + "/" + (facts.Executable.Length > 0
            ? facts.Executable.Replace('\\', '/')
            : facts.AssemblyName + (windowsTarget ? ".exe" : ""));
        var launch = new List<KeyValuePair<string, object>> { Pair("executable", executable) };
        if (facts.Arguments.Count > 0)
        {
            launch.Add(Pair("arguments", facts.Arguments.Cast<object>().ToList()));
        }
        var hasCommand = facts.Command.Trim().Length > 0;
        if (Flag(facts.KeepWorkingDirectory) ?? hasCommand)
        {
            launch.Add(Pair("keepWorkingDirectory", true));
        }

        var project = new List<KeyValuePair<string, object>>
        {
            Pair("application", application),
            Pair("launch", launch),
        };

        if (hasCommand)
        {
            project.Add(Pair("command", new List<KeyValuePair<string, object>> { Pair("name", facts.Command.Trim()) }));
        }

        var desktop = new List<KeyValuePair<string, object>>
        {
            Pair("shortcut", Flag(facts.Shortcut) ?? true),
            Pair("terminal", Terminal(facts, result)),
        };
        if (facts.IconSource.Length > 0)
        {
            var extension = Path.GetExtension(facts.IconSource).ToLowerInvariant();
            result.IconPayloadPath = PayloadFolder + "/icon" + extension;
            desktop.Add(Pair("icon", result.IconPayloadPath));
        }
        project.Add(Pair("desktop", desktop));

        if (facts.UpdateUrl.Trim().Length > 0)
        {
            var update = new List<KeyValuePair<string, object>>
            {
                Pair("url", facts.UpdateUrl.Trim().Replace("{platform}", result.Platform)),
            };
            AddIfSet(update, "channel", facts.UpdateChannel);
            project.Add(Pair("update", update));
        }

        if (Flag(facts.SingleInstance) == true)
        {
            project.Add(Pair("instance", new List<KeyValuePair<string, object>> { Pair("single", true) }));
        }

        result.Json = JsonWriter.Write(project);
        return result;
    }

    /// <summary>
    /// Whether the shortcut opens a terminal for the application.
    /// </summary>
    /// <remarks>
    /// A console tool needs one; a window does not, and gets an empty console
    /// beside it if given one. <c>OutputType</c> alone cannot tell them apart:
    /// Avalonia and MAUI applications are often <c>Exe</c>, so with one of those
    /// and no explicit setting the guess is a window, with a warning.
    /// </remarks>
    private static bool Terminal(ProjectFacts facts, ManifestResult result)
    {
        var explicitly = Flag(facts.Terminal);
        if (explicitly.HasValue)
        {
            return explicitly.Value;
        }
        if (facts.UsesWpf || facts.UsesWindowsForms
            || string.Equals(facts.OutputType, "WinExe", StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }
        if (facts.UsesAvaloniaOrMaui)
        {
            result.Warnings.Add(
                "this project uses Avalonia or MAUI with OutputType Exe; packaging it as a window "
                + "(no terminal). Set <XPackTerminal>true</XPackTerminal> if it is a console tool, "
                + "or false to silence this.");
            return false;
        }
        return true;
    }

    /// <summary><c>true</c>, <c>false</c>, or unset, from an MSBuild property.</summary>
    internal static bool? Flag(string value)
    {
        var trimmed = value.Trim();
        if (trimmed.Length == 0)
        {
            return null;
        }
        return string.Equals(trimmed, "true", StringComparison.OrdinalIgnoreCase);
    }

    private static string FirstOf(params string[] values) =>
        values.Select(v => v.Trim()).FirstOrDefault(v => v.Length > 0) ?? "";

    private static void AddIfSet(List<KeyValuePair<string, object>> target, string key, string value)
    {
        var trimmed = value.Trim();
        if (trimmed.Length > 0)
        {
            target.Add(Pair(key, trimmed));
        }
    }

    private static KeyValuePair<string, object> Pair(string key, object value) =>
        new KeyValuePair<string, object>(key, value);
}
