using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.RegularExpressions;
using Microsoft.Build.Framework;
using Microsoft.Build.Utilities;

namespace XPack.Build;

/// <summary>A SemVer version, ordered as SemVer orders them.</summary>
/// <remarks>
/// Only to choose which earlier releases deltas are built from: whether a
/// version is valid is the xpack command line's to decide, and anything this
/// cannot read is simply not a candidate.
/// </remarks>
internal sealed class SemVer : IComparable<SemVer>
{
    private static readonly Regex Grammar = new Regex(
        @"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$");

    private readonly long[] core;
    private readonly string[] pre;

    private SemVer(long[] core, string[] pre)
    {
        this.core = core;
        this.pre = pre;
    }

    public static SemVer? Parse(string text)
    {
        var match = Grammar.Match(text.Trim());
        if (!match.Success)
        {
            return null;
        }
        var core = new[] { 1, 2, 3 }.Select(i => long.Parse(match.Groups[i].Value, System.Globalization.CultureInfo.InvariantCulture)).ToArray();
        var pre = match.Groups[4].Success ? match.Groups[4].Value.Split('.') : Array.Empty<string>();
        return new SemVer(core, pre);
    }

    public int CompareTo(SemVer? other)
    {
        if (other is null)
        {
            return 1;
        }
        for (var i = 0; i < 3; i++)
        {
            var c = core[i].CompareTo(other.core[i]);
            if (c != 0)
            {
                return c;
            }
        }
        // A release outranks its pre-releases.
        if (pre.Length == 0 || other.pre.Length == 0)
        {
            return other.pre.Length.CompareTo(pre.Length) switch { 0 => 0, var c => Math.Sign(c) };
        }
        for (var i = 0; i < Math.Min(pre.Length, other.pre.Length); i++)
        {
            var mineNumeric = long.TryParse(pre[i], out var mine);
            var theirsNumeric = long.TryParse(other.pre[i], out var theirs);
            var c = (mineNumeric, theirsNumeric) switch
            {
                (true, true) => mine.CompareTo(theirs),
                (true, false) => -1,
                (false, true) => 1,
                _ => string.CompareOrdinal(pre[i], other.pre[i]),
            };
            if (c != 0)
            {
                return Math.Sign(c);
            }
        }
        return pre.Length.CompareTo(other.pre.Length);
    }
}

/// <summary>What a package says it is, as <c>xpack inspect --json</c> reports it.</summary>
internal sealed class PackageIdentity
{
    public string Id { get; set; } = "";
    public string Version { get; set; } = "";
    public string Platform { get; set; } = "";

    /// <summary>Reads the identity from <c>xpack inspect --json</c>; null when it is not there.</summary>
    public static PackageIdentity? FromInspect(string json)
    {
        var application = JsonText.Object(json, "application");
        var platform = JsonText.Object(json, "platform");
        if (application == null || platform == null)
        {
            return null;
        }
        var identity = new PackageIdentity
        {
            Id = RunXPack.ReadStringField(application, "id"),
            Version = RunXPack.ReadStringField(application, "version"),
            Platform = RunXPack.ReadStringField(platform, "os") + "-" + RunXPack.ReadStringField(platform, "arch"),
        };
        return identity.Id.Length == 0 || identity.Version.Length == 0 ? null : identity;
    }
}

/// <summary>Just enough reading of JSON text to find one object inside another.</summary>
internal static class JsonText
{
    /// <summary>The text of the object a top-level <paramref name="key"/> holds, braces included; null when absent.</summary>
    public static string? Object(string json, string key)
    {
        var match = Regex.Match(json, "\"" + Regex.Escape(key) + "\"\\s*:\\s*\\{");
        if (!match.Success)
        {
            return null;
        }
        var start = match.Index + match.Length - 1;
        var depth = 0;
        var inString = false;
        for (var i = start; i < json.Length; i++)
        {
            var c = json[i];
            if (inString)
            {
                if (c == '\\')
                {
                    i++;
                }
                else if (c == '"')
                {
                    inString = false;
                }
                continue;
            }
            switch (c)
            {
                case '"': inString = true; break;
                case '{': depth++; break;
                case '}':
                    depth--;
                    if (depth == 0)
                    {
                        return json.Substring(start, i - start + 1);
                    }
                    break;
            }
        }
        return null;
    }
}

/// <summary>
/// Builds a delta to this release from each of the most recent earlier ones.
/// </summary>
/// <remarks>
/// The earlier packages are the ones users have installed, byte for byte, so
/// they come from a folder the publisher fills from wherever their releases
/// live: a GitHub release, a download server. A delta from anything else would
/// not apply. Which of them are earlier releases of this application for this
/// platform is read from each package itself, with <c>xpack inspect</c>.
/// </remarks>
public sealed class BuildXPackDeltas : Task
{
    [Required] public string Executable { get; set; } = "";
    [Required] public string Package { get; set; } = "";
    [Required] public string Id { get; set; } = "";
    [Required] public string Platform { get; set; } = "";
    [Required] public string Version { get; set; } = "";
    [Required] public string OutputDirectory { get; set; } = "";
    public string PreviousPackages { get; set; } = "";
    public int Count { get; set; } = 3;

    [Output] public ITaskItem[] Deltas { get; private set; } = Array.Empty<ITaskItem>();

    /// <summary><c>--delta &lt;file&gt;</c> for each delta, in order, for <c>xpack index</c>.</summary>
    [Output] public ITaskItem[] IndexArguments { get; private set; } = Array.Empty<ITaskItem>();

    public override bool Execute()
    {
        if (PreviousPackages.Length == 0)
        {
            Log.LogMessage(MessageImportance.High,
                "xpack: no XPackPreviousPackages, so no deltas: every user downloads the full package.");
            return true;
        }
        if (!Directory.Exists(PreviousPackages))
        {
            Log.LogError($"xpack: XPackPreviousPackages names {PreviousPackages}, which does not exist.");
            return false;
        }

        var current = SemVer.Parse(Version);
        var earlier = new List<(SemVer Version, string Path)>();
        foreach (var candidate in Directory.GetFiles(PreviousPackages, "*.xpkg").OrderBy(p => p, StringComparer.Ordinal))
        {
            if (!RunXPack.Run(Log, Executable, new List<string> { "inspect", "--json", candidate }, out var json, out var exit, readingOnly: true))
            {
                return false;
            }
            var identity = exit == 0 ? PackageIdentity.FromInspect(json) : null;
            var version = identity == null ? null : SemVer.Parse(identity.Version);
            if (identity == null || version == null || identity.Id != Id || identity.Platform != Platform)
            {
                continue;
            }
            if (current != null && version.CompareTo(current) < 0 && earlier.All(e => e.Version.CompareTo(version) != 0))
            {
                earlier.Add((version, candidate));
            }
        }

        var chosen = earlier.OrderByDescending(e => e.Version).Take(Math.Max(Count, 0)).ToList();
        if (chosen.Count == 0)
        {
            Log.LogWarning(
                $"xpack: no earlier release of {Id} for {Platform} in {PreviousPackages}, so no deltas: "
                + "every user downloads the full package. Expected for a first release.");
            return true;
        }

        var built = new List<ITaskItem>();
        foreach (var (_, from) in chosen)
        {
            var arguments = new List<string> { "delta", from, Package, "--out-dir", OutputDirectory, "--json" };
            if (!RunXPack.Run(Log, Executable, arguments, out var report, out var exit))
            {
                return false;
            }
            if (exit != 0)
            {
                Log.LogError($"xpack: building the delta from {Path.GetFileName(from)} failed; its own message is above");
                return false;
            }
            var delta = RunXPack.ReadStringField(report, "delta");
            Log.LogMessage(MessageImportance.High, "xpack: delta " + delta);
            built.Add(new TaskItem(delta));
        }
        Deltas = built.ToArray();
        IndexArguments = built.SelectMany(d => new ITaskItem[] { new TaskItem("--delta"), new TaskItem(d.ItemSpec) }).ToArray();
        return true;
    }
}
