namespace XivLantern.Core;

public sealed record FriendFolder(string Name, IReadOnlyList<FriendView> Friends)
{
    public int Online => Friends.Count(f => f.Online);
}

/// <summary>Folder assignments and mutes are local configuration, never protocol payloads.</summary>
public static class FriendFolders
{
    public static Dictionary<string, string> Normalize(IReadOnlyDictionary<string, string>? assignments)
    {
        var result = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (var pair in assignments ?? new Dictionary<string, string>())
        {
            var name = (pair.Value ?? "").Trim();
            if (pair.Key.Length == 64 && pair.Key.All(Uri.IsHexDigit) && ValidName(name))
                result[pair.Key] = name;
        }
        return result;
    }

    private static bool ValidName(string name) => name.Length > 0 && ChatText.Utf8Length(name) <= 64 && !name.Any(char.IsControl);

    public static Dictionary<string, string> Assign(IReadOnlyDictionary<string, string> assignments,
        string nodeId, string name, out string error)
    {
        var result = Normalize(assignments);
        name = name.Trim();
        error = "";
        if (nodeId.Length != 64 || !nodeId.All(Uri.IsHexDigit)) { error = "Invalid friend identity."; return result; }
        if (name.Length == 0) { result.Remove(nodeId); return result; }
        if (!ValidName(name)) { error = "Folder names must fit 64 UTF-8 bytes and contain no control characters."; return result; }
        result[nodeId] = name;
        return result;
    }

    public static IReadOnlyList<FriendFolder> Group(IReadOnlyList<FriendView> friends, IReadOnlyDictionary<string, string> assignments)
        => friends.GroupBy(f => assignments.TryGetValue(f.Id, out var name) ? name : "", StringComparer.OrdinalIgnoreCase)
            .OrderBy(g => g.Key.Length == 0).ThenBy(g => g.Key, StringComparer.OrdinalIgnoreCase)
            .Select(g => new FriendFolder(g.Key, g.ToArray())).ToArray();

    public static HashSet<string> SetMuted(IReadOnlySet<string> muted, FriendFolder folder, bool value)
    {
        var result = new HashSet<string>(muted);
        foreach (var friend in folder.Friends)
        {
            var key = ChatBook.FriendKey(friend.Id);
            if (value) result.Add(key); else result.Remove(key);
        }
        return result;
    }
}
