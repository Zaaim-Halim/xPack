using System;
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
        version = "0.8.1-test." + DateTime.UtcNow.ToString("yyyyMMddHHmmssfff");
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
    private string Application(string extraProperties = "")
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
        File.WriteAllText(Path.Combine(app, "Program.cs"),
            "System.Console.WriteLine($\"hello from xpack, args=[{string.Join(\"|\", args)}]\");\n");
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
        Assert.Contains("builds an installer for the machine it runs on", publishOutput);
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
}
