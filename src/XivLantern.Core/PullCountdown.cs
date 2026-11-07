using System.Globalization;

namespace XivLantern.Core;

public static class PullEnvelope
{
    public const string Prefix = "[Lantern pull v1] countdown: ";
    public static string Encode(int seconds) => seconds is >= 1 and <= 60
        ? Prefix + seconds.ToString(CultureInfo.InvariantCulture) + "s"
        : throw new ArgumentOutOfRangeException(nameof(seconds));

    public static int? Parse(string text)
    {
        if (!text.StartsWith(Prefix, StringComparison.Ordinal) || !text.EndsWith('s')) return null;
        var number = text.AsSpan(Prefix.Length, text.Length - Prefix.Length - 1);
        return int.TryParse(number, NumberStyles.None, CultureInfo.InvariantCulture, out var seconds)
            && seconds is >= 1 and <= 60 && text == Encode(seconds) ? seconds : null;
    }
}

public sealed class PullCountdown
{
    private long? deadline;
    private bool completed;
    public string Channel { get; private set; } = "";
    public string Label { get; private set; } = "";
    public bool Start(int seconds, string channel, string label, long now)
    {
        if (seconds is < 1 or > 60 || channel.Length == 0 || deadline is { } end && now < end) return false;
        deadline = now + seconds * 1000L;
        Channel = channel;
        Label = label;
        completed = false;
        return true;
    }

    public int? Remaining(long now) => deadline is { } end && now <= end + 2000
        ? (int)Math.Max(0, (end - now + 999) / 1000) : null;

    /// <summary>Only one completion, never a late toast after a suspended client resumes.</summary>
    public bool Advance(long now)
    {
        if (deadline is not { } end || now < end || completed) return false;
        completed = true;
        return now <= end + 2000;
    }

    public void Clear() { deadline = null; Channel = Label = ""; completed = false; }
}

public static class PullChannels
{
    public static string? Resolve(IEnumerable<(string Key, string Name)> channels, string requested, string? current)
    {
        var all = channels.ToArray();
        if (requested.Length == 0)
        {
            var selected = all.Where(c => c.Key == current).ToArray();
            return selected.Length == 1 ? selected[0].Key : all.Length == 1 ? all[0].Key : null;
        }
        var matches = all.Where(c => string.Equals(c.Name, requested, StringComparison.OrdinalIgnoreCase)).ToArray();
        if (matches.Length == 0 && requested.StartsWith('#'))
            matches = all.Where(c => string.Equals(c.Name, requested[1..], StringComparison.OrdinalIgnoreCase)).ToArray();
        return matches.Length == 1 ? matches[0].Key : null;
    }
}
