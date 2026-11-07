namespace XivLantern.Core;

public sealed record DtrRoster(string Text, string Tooltip)
{
    public const int MaxFriends = 8;

    public static DtrRoster Build(IReadOnlyList<FriendView> friends, int unread, bool hideNames)
    {
        unread = Math.Max(0, unread);
        var online = friends.Count(f => f.Online);
        var text = $"Lantern {online}" + (unread > 0 ? $" · {unread} unread" : "");
        var lines = new List<string> { $"Lantern: {online} friend(s) online, {unread} unread. Click to open." };
        if (!hideNames)
        {
            var sorted = friends.OrderByDescending(f => f.Online)
                .ThenBy(f => f.Display, StringComparer.OrdinalIgnoreCase).ToArray();
            foreach (var friend in sorted.Take(MaxFriends))
            {
                var name = ChatText.Label(ChatText.Preview(friend.Display, 40));
                var status = !friend.Online ? "offline" : friend.Status switch
                {
                    "away" => "away", "busy" => "busy", _ => "online",
                };
                var note = friend.Online ? ChatText.Preview(friend.Note, 80) : "";
                lines.Add($"{name}, {status}" + (note.Length > 0 ? ": " + note : ""));
            }
            if (sorted.Length > MaxFriends) lines.Add($"and {sorted.Length - MaxFriends} more");
        }
        return new(text, string.Join('\n', lines));
    }
}
