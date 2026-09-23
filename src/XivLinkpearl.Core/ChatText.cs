using System.Text;

namespace XivLinkpearl.Core;

/// <summary>Limits and clean-up for text that goes out to, or comes in from, other players.</summary>
public static class ChatText
{
    /// <summary>LP_MAX_CHAT: the largest 1:1 text or support message, in UTF-8 bytes.</summary>
    public const int MaxChatBytes = 2048;

    /// <summary>What a channel line may be. Below LP_MAX_ROOM_MESSAGE (6144): channels are chat too.</summary>
    public const int MaxChannelBytes = 2048;

    /// <summary>The longest display name or note the native layer keeps (characters).</summary>
    public const int MaxName = 64;
    public const int MaxNote = 256;

    public static int Utf8Length(string text) => Encoding.UTF8.GetByteCount(text);

    /// <summary>Cut <paramref name="text"/> to at most <paramref name="maxBytes"/> UTF-8 bytes, never inside a character.</summary>
    public static string Clamp(string text, int maxBytes)
    {
        if (Encoding.UTF8.GetByteCount(text) <= maxBytes)
            return text;
        var sb = new StringBuilder();
        int used = 0;
        foreach (var rune in text.EnumerateRunes())
        {
            int n = rune.Utf8SequenceLength;
            if (used + n > maxBytes)
                break;
            sb.Append(rune.ToString());
            used += n;
        }

        return sb.ToString();
    }

    /// <summary>Other people's text before it reaches ImGui or chat: no control characters except newlines, trimmed.</summary>
    public static string Clean(string text)
    {
        var sb = new StringBuilder(text.Length);
        foreach (char c in text)
        {
            if (c == '\n' || !char.IsControl(c))
                sb.Append(c);
        }

        return sb.ToString().Trim();
    }

    /// <summary>One short line for a toast or the server info bar.</summary>
    public static string Preview(string text, int maxChars = 80)
    {
        var one = Clean(text).Replace('\n', ' ');
        return one.Length <= maxChars ? one : one[..(maxChars - 1)].TrimEnd() + "…";
    }

    /// <summary>ImGui treats '%' in some calls and "##" in labels specially; text shown as a label goes through this.</summary>
    public static string Label(string text) => Clean(text).Replace("##", "# #");
}
