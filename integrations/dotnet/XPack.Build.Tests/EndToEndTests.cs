using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using Xunit;
using Xunit.Abstractions;

namespace XPack.Build.Tests;

/// <summary>
/// The package as a developer uses it: packed to a local feed, referenced by
/// a fresh console application, published, and the result installed and run
/// by the real xpack command line.
/// </summary>
/// <remarks>
/// Needs the xpack command line: <c>XPACK_HOME</c>, or the repository's own
/// <c>target/debug</c> once <c>cargo build</c> has made it. Without one the
/// tests say they were skipped rather than passing quietly.
/// </remarks>
public sealed class EndToEndTests : IDisposable
{
    private readonly ITestOutputHelper output;
    private readonly string work;
    private readonly string version;

    public EndToEndTests(ITestOutputHelper output)
    {
        this.output = output;
        work = Path.Combine(Path.GetTempPath(), "xpack-dotnet-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(work);
        // A version of its own for each run, so NuGet's cache never hands a
        // test an earlier build of the package.
        version = "0.9.0-test." + DateTime.UtcNow.ToString("yyyyMMddHHmmssfff");
    }

    public void Dispose()
    {
        try { Directory.Delete(work, recursive: true); } catch (IOException) { }
        var cached = Path.Combine(
            Environment.GetEnvironmentVariable("NUGET_PACKAGES")
                ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".nuget", "packages"),
            "xpack.build", version);
        try { Directory.Delete(cached, recursive: true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
    }

    private static string RepositoryRoot()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory != null && !File.Exists(Path.Combine(directory.FullName, "Cargo.toml")))
        {
            directory = directory.Parent;
        }
        return directory?.FullName ?? throw new InvalidOperationException("not inside the xPack repository");
    }

    /// <summary>
    /// The folder holding the xpack command line, or null to skip.
    /// </summary>
    /// <remarks>
    /// With <c>XPACK_DOTNET_REQUIRE_XPACK</c> set, as CI sets it, a missing
    /// xpack fails instead: a skip is not a failure, so a broken setup would
    /// otherwise pass these tests by never running them.
    /// </remarks>
    private static string? XPackHome()
    {
        var home = Environment.GetEnvironmentVariable("XPACK_HOME")
            ?? Path.Combine(RepositoryRoot(), "target", "debug");
        var executable = Path.Combine(home, OperatingSystem.IsWindows() ? "xpack.exe" : "xpack");
        if (File.Exists(executable))
        {
            return home;
        }
        Assert.True(Environment.GetEnvironmentVariable("XPACK_DOTNET_REQUIRE_XPACK") is null or "",
            $"no xpack at {executable}, and XPACK_DOTNET_REQUIRE_XPACK forbids skipping");
        return null;
    }

    private static string HostRuntimeIdentifier()
    {
        var arch = RuntimeInformation.OSArchitecture == Architecture.Arm64 ? "arm64" : "x64";
        if (OperatingSystem.IsWindows()) return "win-" + arch;
        if (OperatingSystem.IsMacOS()) return "osx-" + arch;
        return "linux-" + arch;
    }

    private (int ExitCode, string Output) Run(string file, string arguments, string directory)
    {
        var start = new ProcessStartInfo(file, arguments)
        {
            WorkingDirectory = directory,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
        };
        using var process = Process.Start(start)!;
        var stdout = process.StandardOutput.ReadToEndAsync();
        var stderr = process.StandardError.ReadToEndAsync();
        process.WaitForExit();
        var text = stdout.Result + stderr.Result;
        output.WriteLine($"$ {file} {arguments}\n{text}");
        return (process.ExitCode, text);
    }

    private static string Dotnet() =>
        Environment.GetEnvironmentVariable("DOTNET_HOST_PATH") ?? "dotnet";

    /// <summary>Packs the package under test and makes an application that uses it.</summary>
    private string Application(string extraProperties = "", string? program = null)
    {
        var feed = Path.Combine(work, "feed");
        var project = Path.Combine(RepositoryRoot(), "integrations", "dotnet", "XPack.Build", "XPack.Build.csproj");
        var (packed, packOutput) = Run(Dotnet(), $"pack \"{project}\" -c Release -o \"{feed}\" -p:Version={version}", work);
        Assert.True(packed == 0, packOutput);

        var app = Path.Combine(work, "app");
        Directory.CreateDirectory(app);
        File.WriteAllText(Path.Combine(app, "nuget.config"), $"""
            <?xml version="1.0" encoding="utf-8"?>
            <configuration>
              <packageSources>
                <add key="local" value="{feed}" />
                <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
              </packageSources>
            </configuration>
            """);
        File.WriteAllText(Path.Combine(app, "Hello.csproj"), $"""
            <Project Sdk="Microsoft.NET.Sdk">
              <PropertyGroup>
                <OutputType>Exe</OutputType>
                <TargetFramework>net10.0</TargetFramework>
                <Product>Hello "Dotnet"</Product>
                <Version>1.2.0</Version>
                <Company>Example Ltd</Company>
                <XPackId>com.example.hellodotnet</XPackId>
                {extraProperties}
              </PropertyGroup>
              <ItemGroup>
                <PackageReference Include="XPack.Build" Version="{version}" PrivateAssets="all" />
              </ItemGroup>
            </Project>
            """);
        File.WriteAllText(Path.Combine(app, "Program.cs"), program
            ?? "System.Console.WriteLine($\"hello from xpack, args=[{string.Join(\"|\", args)}]\");\n");
        return app;
    }

    private string Key(string xpack)
    {
        var key = Path.Combine(work, "keys", "signing.json");
        var (made, text) = Run(xpack, $"keygen --out \"{key}\"", work);
        Assert.True(made == 0, text);
        return key;
    }

    [Fact]
    public void A_published_application_installs_and_runs_from_its_package()
    {
        var home = XPackHome();
        if (home == null)
        {
            output.WriteLine("skipped: build xpack (cargo build) or set XPACK_HOME to run this");
            return;
        }
        var xpack = Path.Combine(home, OperatingSystem.IsWindows() ? "xpack.exe" : "xpack");
        var app = Application();
        var key = Key(xpack);
        var rid = HostRuntimeIdentifier();

        var (published, publishOutput) = Run(Dotnet(),
            $"publish -c Release -r {rid} --self-contained -p:XPackKey=\"{key}\" -p:XPackHome=\"{home}\"", app);
        Assert.True(published == 0, publishOutput);

        var package = Assert.Single(Directory.GetFiles(Path.Combine(app, "bin", "xpack"), "*.xpkg"));
        var pub = Path.ChangeExtension(key, null) + ".pub.json";
        var (verified, verifyOutput) = Run(xpack, $"verify \"{package}\" --key \"{pub}\"", work);
        Assert.True(verified == 0, verifyOutput);

        var root = Path.Combine(work, "root");
        var (installed, installOutput) = Run(xpack, $"install \"{package}\" --root \"{root}\" --trust \"{pub}\"", work);
        Assert.True(installed == 0, installOutput);

        var (ran, runOutput) = Run(xpack, $"run com.example.hellodotnet --root \"{root}\" -- one \"two words\"", work);
        Assert.True(ran == 0, runOutput);
        Assert.Contains("hello from xpack, args=[one|two words]", runOutput);

        // The name with quotes in it went through xpack.json intact.
        var (inspected, inspectOutput) = Run(xpack, $"inspect --json \"{package}\"", work);
        Assert.True(inspected == 0, inspectOutput);
        Assert.Contains("Hello \\\"Dotnet\\\"", inspectOutput);
        // Debug symbols are left out unless asked for.
        Assert.DoesNotContain(".pdb", inspectOutput);
    }

    [Fact]
    public void A_plain_build_packs_nothing()
    {
        var home = XPackHome();
        if (home == null)
        {
            output.WriteLine("skipped: build xpack (cargo build) or set XPACK_HOME to run this");
            return;
        }
        var app = Application();
        var (built, buildOutput) = Run(Dotnet(), "build -c Release", app);
        Assert.True(built == 0, buildOutput);
        Assert.False(Directory.Exists(Path.Combine(app, "bin", "xpack")), "a plain build made a package");
    }

    [Fact]
    public void A_wrong_xpack_home_is_reported_before_publishing()
    {
        var app = Application();
        var key = Path.Combine(work, "key.json");
        File.WriteAllText(key, "{}");
        var nowhere = Path.Combine(work, "no-xpack-here");
        var (published, publishOutput) = Run(Dotnet(),
            $"publish -c Release -r {HostRuntimeIdentifier()} --self-contained -p:XPackKey=\"{key}\" -p:XPackHome=\"{nowhere}\"", app);
        Assert.NotEqual(0, published);
        Assert.Contains("there is no xpack command line", publishOutput);
        Assert.False(Directory.Exists(Path.Combine(app, "bin", "Release", "net10.0", HostRuntimeIdentifier(), "publish")),
            "it published before saying so");
    }

    /// <summary>The program to run inside a built installer: the file itself, or the macOS bundle's executable.</summary>
    private static string InstallerProgram(string installer)
    {
        if (!Directory.Exists(installer))
        {
            return installer;
        }
        return Assert.Single(Directory.GetFiles(Path.Combine(installer, "Contents", "MacOS")));
    }

    [Fact]
    public void An_installer_built_with_the_package_installs_silently_and_the_application_runs()
    {
        var home = XPackHome();
        if (home == null)
        {
            output.WriteLine("skipped: build xpack (cargo build) or set XPACK_HOME to run this");
            return;
        }
        var xpack = Path.Combine(home, OperatingSystem.IsWindows() ? "xpack.exe" : "xpack");
        var app = Application("<XPackInstaller>true</XPackInstaller>");
        var key = Key(xpack);

        var (published, publishOutput) = Run(Dotnet(),
            $"publish -c Release -r {HostRuntimeIdentifier()} --self-contained -p:XPackKey=\"{key}\" -p:XPackHome=\"{home}\"", app);
        Assert.True(published == 0, publishOutput);

        var dist = Path.Combine(app, "bin", "xpack");
        var installer = Directory.GetFileSystemEntries(dist).Single(p => !p.EndsWith(".xpkg", StringComparison.Ordinal));
        // By name: the build reports the real path, which on macOS is
        // /private/var/… for a temporary folder this test sees as /var/….
        Assert.Contains("xpack: installer ", publishOutput);
        Assert.Contains(Path.GetFileName(installer), publishOutput);

        var root = Path.Combine(work, "installed");
        var (installed, installOutput) = Run(InstallerProgram(installer), $"--silent --root \"{root}\"", work);
        Assert.True(installed == 0, installOutput);

        var (ran, runOutput) = Run(xpack, $"run com.example.hellodotnet --root \"{root}\"", work);
        Assert.True(ran == 0, runOutput);
        Assert.Contains("hello from xpack", runOutput);
    }

    [Fact]
    public void An_installer_for_another_platform_is_refused_before_publishing()
    {
        var home = XPackHome();
        if (home == null)
        {
            output.WriteLine("skipped: build xpack (cargo build) or set XPACK_HOME to run this");
            return;
        }
        var xpack = Path.Combine(home, OperatingSystem.IsWindows() ? "xpack.exe" : "xpack");
        var other = OperatingSystem.IsWindows() ? "osx-arm64" : "win-x64";
        var app = Application("<XPackInstaller>true</XPackInstaller>");
        var key = Key(xpack);

        var (published, publishOutput) = Run(Dotnet(),
            $"publish -c Release -r {other} --self-contained -p:XPackKey=\"{key}\" -p:XPackHome=\"{home}\"", app);
        Assert.NotEqual(0, published);
        // From Windows every other platform is a Unix one, refused for the
        // reason that comes first: Windows cannot make its packages at all.
        Assert.Contains(
            OperatingSystem.IsWindows()
                ? "cannot be built on Windows"
                : "set XPackTargetBinaries to its folder",
            publishOutput);
        Assert.False(Directory.Exists(Path.Combine(app, "bin", "xpack")), "it packed before saying so");
    }

    [Fact]
    public void A_windows_installer_that_could_not_be_signed_is_built_unsigned_with_a_warning()
    {
        var home = XPackHome();
        if (home == null || !OperatingSystem.IsWindows())
        {
            output.WriteLine("skipped: needs Windows and the xpack command line");
            return;
        }
        var xpack = Path.Combine(home, "xpack.exe");
        var app = Application("""
            <XPackInstaller>true</XPackInstaller>
            <XPackWindowsSignCommand>no-such-signing-tool sign {file}</XPackWindowsSignCommand>
            """);
        var key = Key(xpack);

        var (published, publishOutput) = Run(Dotnet(),
            $"publish -c Release -r win-x64 --self-contained -p:XPackKey=\"{key}\" -p:XPackHome=\"{home}\"", app);
        Assert.True(published == 0, publishOutput);
        Assert.Contains("no-such-signing-tool was not found", publishOutput);
        Assert.Contains("NOT code signed", publishOutput);
        Assert.Single(Directory.GetFiles(Path.Combine(app, "bin", "xpack"), "*-Setup.exe"));
    }

    /// <summary>The first bytes of a Linux program for <paramref name="arch"/>.</summary>
    private static byte[] Elf(string arch)
    {
        var bytes = new byte[64];
        "\u007fELF"u8.CopyTo(bytes);
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        BitConverter.GetBytes((ushort)(arch == "arm64" ? 0xB7 : 0x3E)).CopyTo(bytes, 0x12);
        return bytes;
    }

    /// <summary>The programs an appended installer carries, by name, with their first bytes.</summary>
    private static Dictionary<string, byte[]> Carried(string installer)
    {
        var bytes = File.ReadAllBytes(installer);
        var trailer = bytes.AsSpan(bytes.Length - 52);
        Assert.Equal("XPACKBDL"u8.ToArray(), trailer[..8].ToArray());
        var length = (int)BitConverter.ToUInt64(trailer.Slice(12, 8));
        using var archive = new System.IO.Compression.ZipArchive(
            new MemoryStream(bytes, bytes.Length - 52 - length, length));
        var programs = new Dictionary<string, byte[]>();
        foreach (var entry in archive.Entries.Where(e => e.FullName.StartsWith("bin/", StringComparison.Ordinal)))
        {
            using var stream = entry.Open();
            var head = new byte[4];
            stream.ReadExactly(head);
            programs[entry.FullName["bin/".Length..]] = head;
        }
        return programs;
    }

    [Fact]
    public void An_installer_for_another_platform_is_built_from_its_release_folder()
    {
        var home = XPackHome();
        if (home == null || OperatingSystem.IsWindows())
        {
            // From Windows every other platform is a Unix one, whose packages
            // Windows cannot make at all.
            output.WriteLine("skipped: needs macOS or Linux and the xpack command line");
            return;
        }
        var xpack = Path.Combine(home, "xpack");
        var (rid, arch) = HostRuntimeIdentifier() == "linux-x64" ? ("linux-arm64", "arm64") : ("linux-x64", "x64");

        // A stand-in for xPack's release for that platform: real Linux headers,
        // enough for every check, since building an installer runs none of them.
        var release = Path.Combine(work, "xpack-release-" + rid);
        Directory.CreateDirectory(release);
        foreach (var name in new[] { "xpack-installer", "xpack-launcher", "xpack-updater", "xpack-uninstaller", "xpack-hook" })
        {
            File.WriteAllBytes(Path.Combine(release, name), Elf(arch));
        }

        var app = Application($"""
            <XPackInstaller>true</XPackInstaller>
            <XPackTargetBinaries>{release}</XPackTargetBinaries>
            """);
        var key = Key(xpack);
        var (published, publishOutput) = Run(Dotnet(),
            $"publish -c Release -r {rid} --self-contained -p:XPackKey=\"{key}\" -p:XPackHome=\"{home}\"", app);
        Assert.True(published == 0, publishOutput);

        var installer = Assert.Single(Directory.GetFiles(Path.Combine(app, "bin", "xpack"), "*-installer"));
        Assert.Equal(Elf(arch)[..4], File.ReadAllBytes(installer)[..4]);
        var programs = Carried(installer);
        Assert.Equal(new[] { "xpack-hook", "xpack-launcher", "xpack-uninstaller", "xpack-updater" }, programs.Keys.Order());
        Assert.All(programs.Values, head => Assert.Equal(Elf(arch)[..4], head));
    }

    /// <summary>Serves <paramref name="root"/> over loopback HTTP, which the updater allows for exactly this.</summary>
    private static (System.Net.HttpListener Server, string Url) Serve(string root)
    {
        var probe = new System.Net.Sockets.TcpListener(System.Net.IPAddress.Loopback, 0);
        probe.Start();
        var port = ((System.Net.IPEndPoint)probe.LocalEndpoint).Port;
        probe.Stop();
        var url = $"http://127.0.0.1:{port}/";
        var server = new System.Net.HttpListener();
        server.Prefixes.Add(url);
        server.Start();
        _ = System.Threading.Tasks.Task.Run(async () =>
        {
            while (server.IsListening)
            {
                System.Net.HttpListenerContext context;
                try { context = await server.GetContextAsync(); }
                catch (Exception) { return; }
                var relative = Uri.UnescapeDataString(context.Request.Url!.AbsolutePath.TrimStart('/'));
                var file = Path.GetFullPath(Path.Combine(root, relative));
                if (file.StartsWith(Path.GetFullPath(root), StringComparison.Ordinal) && File.Exists(file))
                {
                    var bytes = File.ReadAllBytes(file);
                    context.Response.ContentLength64 = bytes.Length;
                    context.Response.OutputStream.Write(bytes);
                }
                else
                {
                    context.Response.StatusCode = 404;
                }
                context.Response.Close();
            }
        });
        return (server, url);
    }

    [Fact]
    public void A_release_updates_an_installed_copy_through_a_delta()
    {
        var home = XPackHome();
        if (home == null)
        {
            output.WriteLine("skipped: build xpack (cargo build) or set XPACK_HOME to run this");
            return;
        }
        var xpack = Path.Combine(home, OperatingSystem.IsWindows() ? "xpack.exe" : "xpack");
        var rid = HostRuntimeIdentifier();
        var site = Path.Combine(work, "served");
        var (server, url) = Serve(site);
        using var _ = server;

        var app = Application(
            $"<XPackUpdateUrl>{url}updates/{{platform}}</XPackUpdateUrl>",
            "System.Console.WriteLine($\"version {System.Reflection.Assembly.GetEntryAssembly()!.GetName().Version}\");\n");
        var key = Key(xpack);
        var pub = Path.ChangeExtension(key, null) + ".pub.json";
        string Publish(string version, string extra) => Run(Dotnet(),
            $"publish -c Release -r {rid} --self-contained -p:Version={version} -p:XPackKey=\"{key}\" -p:XPackHome=\"{home}\" {extra}", app) is var (code, text) && code == 0
                ? text
                : throw new Xunit.Sdk.XunitException(text);

        // 1.0.0, released earlier and installed.
        Publish("1.0.0", "");
        var previous = Path.Combine(work, "previous");
        Directory.CreateDirectory(previous);
        var first = Assert.Single(Directory.GetFiles(Path.Combine(app, "bin", "xpack"), "*-1.0.0-*.xpkg"));
        File.Copy(first, Path.Combine(previous, Path.GetFileName(first)));
        var root = Path.Combine(work, "root");
        var (installed, installOutput) = Run(xpack, $"install \"{first}\" --root \"{root}\" --trust \"{pub}\"", work);
        Assert.True(installed == 0, installOutput);

        // 1.1.0, released: a delta from 1.0.0, and the index.
        var released = Publish("1.1.0", $"-p:XPackRelease=true -p:XPackPreviousPackages=\"{previous}\"");
        Assert.Contains("-1.0.0-to-1.1.0-", released);
        var platformSite = Path.Combine(app, "bin", "xpack", "site", Manifest.HostPlatform());
        Assert.True(File.Exists(Path.Combine(platformSite, "stable.json")), released);
        Assert.Single(Directory.GetFiles(platformSite, "*.xpkgd"));

        // Published where the 1.0.0 installation looks, and taken from there.
        CopyDirectory(Path.Combine(app, "bin", "xpack", "site"), Path.Combine(site, "updates"));
        var (updated, updateOutput) = Run(xpack, $"update com.example.hellodotnet --root \"{root}\"", work);
        Assert.True(updated == 0, updateOutput);
        // The delta, not the package: what was downloaded is a small part of
        // what a full download would have been.
        var downloaded = long.Parse(System.Text.RegularExpressions.Regex
            .Match(updateOutput, @"package downloaded bytes=(\d+)").Groups[1].Value);
        var full = new FileInfo(Assert.Single(Directory.GetFiles(platformSite, "*.xpkg"))).Length;
        Assert.True(downloaded * 20 < full, $"downloaded {downloaded} bytes of a {full}-byte package: no delta was used");

        var (ran, runOutput) = Run(xpack, $"run com.example.hellodotnet --root \"{root}\"", work);
        Assert.True(ran == 0, runOutput);
        Assert.Contains("version 1.1.0", runOutput);
    }

    [Fact]
    public void A_release_without_earlier_packages_is_indexed_with_no_deltas()
    {
        var home = XPackHome();
        if (home == null)
        {
            output.WriteLine("skipped: build xpack (cargo build) or set XPACK_HOME to run this");
            return;
        }
        var xpack = Path.Combine(home, OperatingSystem.IsWindows() ? "xpack.exe" : "xpack");
        var app = Application("<XPackUpdateUrl>https://updates.example.com/hello/{platform}</XPackUpdateUrl>");
        var key = Key(xpack);
        var (published, publishOutput) = Run(Dotnet(),
            $"publish -c Release -r {HostRuntimeIdentifier()} --self-contained -p:XPackKey=\"{key}\" -p:XPackHome=\"{home}\" -p:XPackRelease=true", app);
        Assert.True(published == 0, publishOutput);
        Assert.Contains("no deltas", publishOutput);
        var platformSite = Path.Combine(app, "bin", "xpack", "site", Manifest.HostPlatform());
        Assert.True(File.Exists(Path.Combine(platformSite, "stable.json")));
        Assert.Single(Directory.GetFiles(platformSite, "*.xpkg"));
    }

    private static void CopyDirectory(string from, string to)
    {
        foreach (var file in Directory.GetFiles(from, "*", SearchOption.AllDirectories))
        {
            var target = Path.Combine(to, Path.GetRelativePath(from, file));
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            File.Copy(file, target, overwrite: true);
        }
    }

    [Fact]
    public void The_example_project_builds_installs_and_runs()
    {
        // The example in integrations/dotnet/example, as its README builds
        // it, against the XPack.Build under test: an example that no longer
        // builds is worse than none.
        var home = XPackHome();
        if (home == null)
        {
            output.WriteLine("skipped: build xpack (cargo build) or set XPACK_HOME to run this");
            return;
        }
        var xpack = Path.Combine(home, OperatingSystem.IsWindows() ? "xpack.exe" : "xpack");
        var source = Path.Combine(RepositoryRoot(), "integrations", "dotnet", "example");
        var example = Path.Combine(work, "example");
        foreach (var file in Directory.GetFiles(source, "*", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(source, file);
            if (relative.Split(Path.DirectorySeparatorChar)[0] is "bin" or "obj" or "packages")
            {
                continue;
            }
            var target = Path.Combine(example, relative);
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            File.Copy(file, target);
        }

        var feed = Path.Combine(work, "feed");
        var project = Path.Combine(RepositoryRoot(), "integrations", "dotnet", "XPack.Build", "XPack.Build.csproj");
        var (packed, packOutput) = Run(Dotnet(), $"pack \"{project}\" -c Release -o \"{feed}\" -p:Version={version}", work);
        Assert.True(packed == 0, packOutput);
        // The example's own nuget.config, pointed at this run's feed.
        var config = Path.Combine(example, "nuget.config");
        File.WriteAllText(config, File.ReadAllText(config).Replace("\"../feed\"", $"\"{feed}\""));

        var key = Key(xpack);
        var (published, publishOutput) = Run(Dotnet(),
            $"publish -c Release -r {HostRuntimeIdentifier()} --self-contained -p:XPackKey=\"{key}\" -p:XPackHome=\"{home}\" -p:XPackInstaller=true -p:XPackBuildVersion={version}",
            example);
        Assert.True(published == 0, publishOutput);
        Assert.DoesNotContain("warning", publishOutput.Replace("0 Warning(s)", ""), StringComparison.OrdinalIgnoreCase);

        var dist = Path.Combine(example, "bin", "xpack");
        Assert.Single(Directory.GetFiles(dist, "xPack-DotNet-Demo-1.0.0-*.xpkg"));
        var installer = Directory.GetFileSystemEntries(dist).Single(p => !p.EndsWith(".xpkg", StringComparison.Ordinal));
        var root = Path.Combine(work, "installed");
        var (installed, installOutput) = Run(InstallerProgram(installer), $"--silent --root \"{root}\"", work);
        Assert.True(installed == 0, installOutput);

        var (ran, runOutput) = Run(xpack, $"run com.example.dotnetdemo --root \"{root}\"", work);
        Assert.True(ran == 0, runOutput);
        Assert.Contains("xPack .NET demo 1.0.0", runOutput);
        Assert.Contains("arguments:   [--greeting=hello]", runOutput);
    }
}
