using System.Collections.Generic;
using System.Globalization;
using System.Text;

namespace XPack.Build;

/// <summary>
/// Writes the small subset of JSON <c>xpack.json</c> needs: objects, arrays,
/// strings and booleans.
/// </summary>
/// <remarks>
/// A writer of its own rather than System.Text.Json: a task assembly cannot
/// bring dependencies with it reliably, because MSBuild loads it into a
/// process that already has its own copy of that library, at a version this
/// package does not choose. Strings are escaped as JSON requires, so names,
/// paths and arguments cannot break out of the value they belong to.
/// </remarks>
internal static class JsonWriter
{
    public static string Write(object value)
    {
        var text = new StringBuilder();
        WriteValue(text, value, 0);
        text.Append('\n');
        return text.ToString();
    }

    private static void WriteValue(StringBuilder text, object value, int depth)
    {
        switch (value)
        {
            case string s:
                WriteString(text, s);
                break;
            case bool b:
                text.Append(b ? "true" : "false");
                break;
            case IEnumerable<KeyValuePair<string, object>> obj:
                WriteObject(text, obj, depth);
                break;
            case IEnumerable<object> list:
                WriteArray(text, list, depth);
                break;
            default:
                throw new System.ArgumentException($"cannot write a {value.GetType().Name} as JSON");
        }
    }

    private static void WriteObject(StringBuilder text, IEnumerable<KeyValuePair<string, object>> obj, int depth)
    {
        text.Append('{');
        var first = true;
        foreach (var pair in obj)
        {
            text.Append(first ? "\n" : ",\n");
            first = false;
            Indent(text, depth + 1);
            WriteString(text, pair.Key);
            text.Append(": ");
            WriteValue(text, pair.Value, depth + 1);
        }
        if (!first)
        {
            text.Append('\n');
            Indent(text, depth);
        }
        text.Append('}');
    }

    private static void WriteArray(StringBuilder text, IEnumerable<object> list, int depth)
    {
        text.Append('[');
        var first = true;
        foreach (var item in list)
        {
            text.Append(first ? "\n" : ",\n");
            first = false;
            Indent(text, depth + 1);
            WriteValue(text, item, depth + 1);
        }
        if (!first)
        {
            text.Append('\n');
            Indent(text, depth);
        }
        text.Append(']');
    }

    internal static void WriteString(StringBuilder text, string value)
    {
        text.Append('"');
        foreach (var c in value)
        {
            switch (c)
            {
                case '"': text.Append("\\\""); break;
                case '\\': text.Append("\\\\"); break;
                case '\b': text.Append("\\b"); break;
                case '\f': text.Append("\\f"); break;
                case '\n': text.Append("\\n"); break;
                case '\r': text.Append("\\r"); break;
                case '\t': text.Append("\\t"); break;
                default:
                    if (c < 0x20)
                    {
                        text.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                    }
                    else
                    {
                        text.Append(c);
                    }
                    break;
            }
        }
        text.Append('"');
    }

    private static void Indent(StringBuilder text, int depth) => text.Append(' ', depth * 2);
}
