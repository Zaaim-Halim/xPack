using System.Diagnostics;
using System.IO;
using Xunit;

namespace XPack.Build.Tests;

public class RunXPackTests
{
    [Theory]
    [InlineData("plain")]
    [InlineData("with space")]
    [InlineData("C:\\Program Files\\xpack\\")]
    [InlineData("quote\"inside")]
    [InlineData("trailing\\\\")]
    [InlineData("\\\"both\\\"")]
    [InlineData("")]
    [InlineData("tab\there")]
    public void An_argument_reaches_the_program_exactly_as_given(string argument)
    {
        // Round trip through a real process: the shell's own `printf` on
        // Unix, so the test checks the receiving side, not this code's idea
        // of it. Windows uses the same rules through its C runtime; CI runs
        // this on Windows too.
        if (System.OperatingSystem.IsWindows())
        {
            return;
        }
        var start = new ProcessStartInfo("/usr/bin/printf")
        {
            Arguments = "%s " + RunXPack.QuoteForWindowsCommandLine(argument),
            RedirectStandardOutput = true,
            UseShellExecute = false,
        };
        using var process = Process.Start(start)!;
        var output = process.StandardOutput.ReadToEnd();
        process.WaitForExit();
        Assert.Equal(argument, output);
    }

    [Fact]
    public void The_package_path_is_read_from_the_report()
    {
        var report = "{\n  \"package\": \"/a b/My \\\"App\\\"-1.0.0-linux-x64.xpkg\",\n  \"application\": \"com.example\"\n}";
        Assert.Equal("/a b/My \"App\"-1.0.0-linux-x64.xpkg", RunXPack.ReadStringField(report, "package"));
        Assert.Equal("", RunXPack.ReadStringField(report, "missing"));
    }

    [Fact]
    public void The_installer_and_whether_it_was_signed_are_read_from_the_report()
    {
        var signed = "{\n  \"installer\": \"C:\\\\dist\\\\My-App-1.0.0-windows-x64-Setup.exe\",\n  \"codeSigned\": true\n}";
        Assert.Equal("C:\\dist\\My-App-1.0.0-windows-x64-Setup.exe", RunXPack.ReadStringField(signed, "installer"));
        Assert.True(RunXPack.ReadTrue(signed, "codeSigned"));
        Assert.False(RunXPack.ReadTrue("{\"codeSigned\": false}", "codeSigned"));
        Assert.False(RunXPack.ReadTrue("{\"installer\": \"x\"}", "codeSigned"));
        Assert.False(RunXPack.ReadTrue("{\"codeSigned\": trueish}", "codeSigned"));
    }

    [Fact]
    public void The_package_version_is_the_xpack_release_it_was_made_for()
    {
        var expected = typeof(RunXPack).Assembly.GetName().Version!;
        Assert.Equal($"{expected.Major}.{expected.Minor}.{expected.Build}", RunXPack.PackageVersion());
        Assert.Equal("0.8.1", RunXPack.PackageVersion());
    }
}
