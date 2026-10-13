namespace XivLantern.Core;

/// <summary>Only player-chosen activities; no game state is consulted.</summary>
public static class ActivityTags
{
    public const int MaxTags = 8;
    public const int MaxTagBytes = 32;
    public static readonly string[] Suggested = ["maps", "roulettes", "crafting", "ex", "chat"];

    public static List<string> Normalize(IEnumerable<string>? tags)
    {
        var result = new List<string>();
        foreach (var value in tags ?? [])
        {
            if (value == null) continue;
            var clean = new string(value.Trim().Where(c => !char.IsControl(c)).ToArray());
            var tag = ChatText.Clamp(clean, MaxTagBytes).Trim();
            if (tag.Length == 0 || result.Contains(tag, StringComparer.OrdinalIgnoreCase)) continue;
            result.Add(tag);
            if (result.Count == MaxTags) break;
        }
        return result;
    }

    public static bool Matches(FriendView friend, string filter) => filter.Length == 0 ||
        friend.Online && friend.Tags.Contains(filter, StringComparer.OrdinalIgnoreCase);
}
