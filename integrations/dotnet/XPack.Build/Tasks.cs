using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using Microsoft.Build.Framework;
using Microsoft.Build.Utilities;

namespace XPack.Build;

/// <summary>Writes <c>xpack.json</c> from the project's properties.</summary>
public sealed class WriteXPackManifest : Task
{
    [Required] public string ManifestPath { get; set; } = "";
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
    public bool UsesMaui { get; set; }
    /// <summary>The project's package references, to recognise UI frameworks by.</summary>
    public ITaskItem[] PackageReferences { get; set; } = Array.Empty<ITaskItem>();
    public string Name { get; set; } = "";
    public string Publisher { get; set; } = "";
    public string Executable { get; set; } = "";
    public ITaskItem[] Arguments { get; set; } = Array.Empty<ITaskItem>();
    public string Command { get; set; } = "";
    public string KeepWorkingDirectory { get; set; } = "";
    public string Terminal { get; set; } = "";
    public string Shortcut { get; set; } = "";
    public string UpdateUrl { get; set; } = "";
    public string UpdateChannel { get; set; } = "";
    public string SingleInstance { get; set; } = "";
    public string IconSource { get; set; } = "";

    /// <summary>The xPack platform, such as <c>windows-x64</c>.</summary>
    [Output] public string Platform { get; private set; } = "";

    /// <summary>Where the icon goes in the payload; empty without one.</summary>
    [Output] public string IconPayloadPath { get; private set; } = "";

    /// <summary>The xPack platform of the machine building, such as <c>macos-arm64</c>.</summary>
    [Output] public string HostPlatform { get; private set; } = "";

    public override bool Execute()
    {
        if (IconSource.Length > 0 && !File.Exists(IconSource))
        {
            Log.LogError($"xpack: the icon {IconSource} does not exist");
            return false;
        }

        var result = Manifest.Build(new ProjectFacts
        {
            Id = Id,
            AssemblyName = AssemblyName,
            Product = Product,
            Version = Version,
            Description = Description,
            Company = Company,
            RuntimeIdentifier = RuntimeIdentifier,
            OutputType = OutputType,
            UsesWpf = UsesWpf,
            UsesWindowsForms = UsesWindowsForms,
            UsesAvaloniaOrMaui = UsesMaui || PackageReferences.Any(r =>
                r.ItemSpec.Equals("Avalonia", StringComparison.OrdinalIgnoreCase)
                || r.ItemSpec.StartsWith("Avalonia.", StringComparison.OrdinalIgnoreCase)),
            HostIsWindows = RuntimeInformation.IsOSPlatform(OSPlatform.Windows),
            Name = Name,
            Publisher = Publisher,
            Executable = Executable,
            Arguments = Arguments.Select(a => a.ItemSpec).ToList(),
            Command = Command,
            KeepWorkingDirectory = KeepWorkingDirectory,
            Terminal = Terminal,
            Shortcut = Shortcut,
            UpdateUrl = UpdateUrl,
            UpdateChannel = UpdateChannel,
            SingleInstance = SingleInstance,
            IconSource = IconSource,
        });

        foreach (var warning in result.Warnings)
        {
            Log.LogWarning("xpack: " + warning);
        }
        foreach (var error in result.Errors)
        {
            Log.LogError("xpack: " + error);
        }
        if (!result.Succeeded)
        {
            return false;
        }

        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(ManifestPath))!);
        // Written only when it changes, so an unchanged project does not
        // look newer to the targets that pack from it.
        var existing = File.Exists(ManifestPath) ? File.ReadAllText(ManifestPath) : null;
        if (existing != result.Json)
        {
            File.WriteAllText(ManifestPath, result.Json, new UTF8Encoding(false));
        }
        Platform = result.Platform;
        IconPayloadPath = result.IconPayloadPath;
        HostPlatform = Manifest.HostPlatform();
        return true;
    }
}

/// <summary>Runs the xpack command line.</summary>
/// <remarks>
/// Each argument is passed on its own, never joined into one command line
/// for a shell to split again, so a path with spaces or quotes means the same
/// on every platform. What xpack prints for a person (stderr) is logged as it
/// arrives; what it prints for a program (stdout) is returned.
/// </remarks>
public sealed class RunXPack : Task
{
    [Required] public string Executable { get; set; } = "";
    [Required] public ITaskItem[] Arguments { get; set; } = Array.Empty<ITaskItem>();

    /// <summary>
    /// Whether to compare <c>xpack --version</c> with the xPack release this
    /// package was made for, and warn when they differ.
    /// </summary>
    public bool CheckVersion { get; set; }

    /// <summary>The <c>package</c> field of xpack's <c>--json</c> report, when it has one.</summary>
    [Output] public string Package { get; private set; } = "";

    /// <summary>The <c>installer</c> field of xpack's <c>--json</c> report, when it has one.</summary>
    [Output] public string Installer { get; private set; } = "";

    /// <summary>Whether xpack's report says the installer was code signed.</summary>
    [Output] public bool CodeSigned { get; private set; }

    [Output] public string StandardOutput { get; private set; } = "";

    public override bool Execute()
    {
        if (CheckVersion && !VersionMatches())
        {
            return false;
        }

        var arguments = Arguments.Select(a => a.ItemSpec).ToList();
        if (!Run(arguments, out var stdout, out var exitCode))
        {
            return false;
        }
        StandardOutput = stdout;
        if (exitCode != 0)
        {
            Log.LogError($"xpack: `xpack {string.Join(" ", arguments.Take(1))}` failed (exit code {exitCode}); its own message is above");
            return false;
        }
        Package = ReadStringField(stdout, "package");
        Installer = ReadStringField(stdout, "installer");
        CodeSigned = ReadTrue(stdout, "codeSigned");
        return true;
    }

    /// <summary>
    /// The xPack release this package was made for: its own version, which is
    /// set to the release it ships with.
    /// </summary>
    internal static string PackageVersion()
    {
        var version = typeof(RunXPack).Assembly.GetName().Version ?? new Version(0, 0, 0);
        return $"{version.Major}.{version.Minor}.{version.Build}";
    }

    private bool VersionMatches()
    {
        var expected = PackageVersion();
        if (!Run(new List<string> { "--version" }, out var stdout, out var exitCode))
        {
            return false;
        }
        var reported = stdout.Trim();
        var version = reported.StartsWith("xpack ", StringComparison.Ordinal) ? reported.Substring(6) : reported;
        if (exitCode != 0 || version.Length == 0)
        {
            Log.LogError($"xpack: {Executable} did not report a version; is it the xpack command line?");
            return false;
        }
        if (version != expected)
        {
            Log.LogWarning(
                $"xpack: this build uses xpack {version}, but XPack.Build {expected} was made for "
                + $"xpack {expected}. Use the matching release, or set XPackHome to it.");
        }
        return true;
    }

    private bool Run(IList<string> arguments, out string stdout, out int exitCode)
    {
        stdout = "";
        exitCode = -1;
        var start = new ProcessStartInfo(Executable)
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true,
        };
        start.Arguments = string.Join(" ", arguments.Select(QuoteForWindowsCommandLine));

        Process process;
        try
        {
            process = Process.Start(start) ?? throw new InvalidOperationException("no process was started");
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception || e is InvalidOperationException)
        {
            Log.LogError(
                $"xpack: could not run {Executable}: {e.Message}. Install xPack and put xpack on the PATH, "
                + "or set XPackHome to the folder that holds it.");
            return false;
        }

        using (process)
        {
            var output = new StringBuilder();
            process.OutputDataReceived += (_, e) => { if (e.Data != null) output.AppendLine(e.Data); };
            process.ErrorDataReceived += (_, e) => { if (e.Data != null) LogLine(e.Data); };
            process.BeginOutputReadLine();
            process.BeginErrorReadLine();
            process.WaitForExit();
            stdout = output.ToString();
            exitCode = process.ExitCode;
        }
        return true;
    }

    /// <summary>
    /// A line xpack printed for a person: a warning stays a warning in the
    /// build's own log, where it is not mistaken for ordinary output.
    /// </summary>
    private void LogLine(string line)
    {
        var trimmed = line.TrimStart();
        if (trimmed.StartsWith("warning:", StringComparison.Ordinal))
        {
            Log.LogWarning("xpack: " + trimmed.Substring("warning:".Length).Trim());
        }
        else if (trimmed.StartsWith("error:", StringComparison.Ordinal))
        {
            Log.LogError("xpack: " + trimmed.Substring("error:".Length).Trim());
        }
        else
        {
            Log.LogMessage(MessageImportance.High, line);
        }
    }

    /// <summary>
    /// Quotes one argument so the receiving program's runtime splits it back
    /// into exactly this string.
    /// </summary>
    /// <remarks>
    /// <c>ProcessStartInfo.ArgumentList</c> does this, but is not in
    /// netstandard2.0. These are the rules the Microsoft C runtime and Rust's
    /// standard library both use to split a Windows command line; on Unix,
    /// .NET splits <c>Arguments</c> by the same rules before exec.
    /// </remarks>
    internal static string QuoteForWindowsCommandLine(string argument)
    {
        if (argument.Length > 0 && argument.IndexOfAny(new[] { ' ', '\t', '\n', '\v', '"' }) < 0)
        {
            return argument;
        }
        var quoted = new StringBuilder("\"");
        var backslashes = 0;
        foreach (var c in argument)
        {
            if (c == '\\')
            {
                backslashes++;
                continue;
            }
            if (c == '"')
            {
                quoted.Append('\\', backslashes * 2 + 1).Append('"');
            }
            else
            {
                quoted.Append('\\', backslashes).Append(c);
            }
            backslashes = 0;
        }
        quoted.Append('\\', backslashes * 2).Append('"');
        return quoted.ToString();
    }

    /// <summary>Whether a field of a JSON object is <c>true</c>; absent or anything else is not.</summary>
    internal static bool ReadTrue(string json, string field) =>
        Regex.IsMatch(json, "\"" + Regex.Escape(field) + "\"\\s*:\\s*true\\b");

    /// <summary>A top-level string field of a JSON object, unescaped; empty when absent.</summary>
    internal static string ReadStringField(string json, string field)
    {
        var match = Regex.Match(json, "\"" + Regex.Escape(field) + "\"\\s*:\\s*\"((?:\\\\.|[^\"\\\\])*)\"");
        if (!match.Success)
        {
            return "";
        }
        return Regex.Replace(match.Groups[1].Value, @"\\(u[0-9a-fA-F]{4}|.)", m =>
        {
            var escape = m.Groups[1].Value;
            switch (escape[0])
            {
                case 'n': return "\n";
                case 'r': return "\r";
                case 't': return "\t";
                case 'b': return "\b";
                case 'f': return "\f";
                case 'u': return ((char)Convert.ToInt32(escape.Substring(1), 16)).ToString();
                default: return escape;
            }
        });
    }
}
