// A deliberately ordinary console application, used to show what xPack gives
// one: where it was installed, how it was started, and with what.

using System.Reflection;
using System.Runtime.InteropServices;

var version = Assembly.GetEntryAssembly()!.GetName().Version!;
Console.WriteLine($"xPack .NET demo {version.Major}.{version.Minor}.{version.Build}");
Console.WriteLine($"  arguments:   [{string.Join(", ", args)}]");
Console.WriteLine($"  runtime:     {RuntimeInformation.FrameworkDescription} on {RuntimeInformation.RuntimeIdentifier}");
Console.WriteLine($"  installed:   {AppContext.BaseDirectory}");
Console.WriteLine($"  started in:  {Environment.CurrentDirectory}");
Console.WriteLine($"  application: {Environment.GetEnvironmentVariable("XPACK_APPLICATION_DIR") ?? "(not started by xPack)"}");

// A version that fails to start is rolled back to the one before it. Start
// it with --crash after an update to watch that happen.
if (args.Contains("--crash"))
{
    Console.WriteLine("exiting with an error on purpose");
    return 3;
}
return 0;
