namespace XivLinkpearl.Core;

public enum LineState
{
    /// <summary>Received, or a system line.</summary>
    None,
    /// <summary>Sent, not acknowledged yet (a friend text waits in the outbox until their link is up).</summary>
    Pending,
    Delivered,
    Failed,
}

/// <summary>One line in a conversation. <see cref="From"/> is empty for our own lines and for system lines.</summary>
public sealed record ChatLine(DateTimeOffset Time, string From, string Text, bool Outgoing, ulong Id = 0, LineState State = LineState.None, bool System = false, bool Verified = false);

/// <summary>A friend chat ("f:&lt;id&gt;"), a channel ("c:&lt;room&gt;"), the author's announcements ("author") or the
/// conversation with the author ("support"). Bounded: the oldest lines fall off; friend history stays in SQLite.</summary>
public sealed class Conversation(string key, string title)
{
    public const int MaxLines = 500;

    private readonly List<ChatLine> lines = [];

    public string Key { get; } = key;
    public string Title { get; set; } = title;
    public int Unread { get; private set; }
    public IReadOnlyList<ChatLine> Lines => lines;

    /// <summary>Changes whenever a line is added or changes state, so a UI can tell it has to scroll.</summary>
    public int Version { get; private set; }

    public void Add(ChatLine line, bool countUnread)
    {
        // A friend text is raised once, but history loaded after the fact may already hold it.
        if (line.Id != 0 && !line.Outgoing && !line.System && lines.Any(l => l.Id == line.Id && !l.Outgoing))
            return;
        lines.Add(line);
        if (lines.Count > MaxLines)
            lines.RemoveRange(0, lines.Count - MaxLines);
        if (countUnread && !line.Outgoing && !line.System)
            Unread++;
        Version++;
    }

    /// <summary>Set the state of our own line with this id; false when there is none (it scrolled away).</summary>
    public bool Mark(ulong id, LineState state)
    {
        int i = lines.FindLastIndex(l => l.Outgoing && l.Id == id);
        if (i < 0)
            return false;
        lines[i] = lines[i] with { State = state };
        Version++;
        return true;
    }

    public void MarkRead() => Unread = 0;

    /// <summary>Replace the lines with history from SQLite (oldest first), keeping anything newer already here.</summary>
    public void LoadHistory(IEnumerable<ChatLine> history)
    {
        var older = history.ToList();
        var ids = older.Where(l => l.Id != 0).Select(l => (l.Id, l.Outgoing)).ToHashSet();
        var keep = lines.Where(l => l.Id == 0 || !ids.Contains((l.Id, l.Outgoing))).ToList();
        lines.Clear();
        lines.AddRange(older);
        lines.AddRange(keep);
        if (lines.Count > MaxLines)
            lines.RemoveRange(0, lines.Count - MaxLines);
        Version++;
    }
}

/// <summary>Every conversation this session, by key.</summary>
public sealed class ChatBook
{
    private readonly Dictionary<string, Conversation> byKey = [];
    private readonly List<string> order = [];

    public const string AuthorKey = "author";
    public const string SupportKey = "support";

    public static string FriendKey(string id) => "f:" + id;
    /// <summary>Keyed by the channel's ticket, which outlives a session's room handle, so mutes stick.</summary>
    public static string ChannelKey(string ticket) => "c:" + ticket;

    public IReadOnlyList<Conversation> All => order.Select(k => byKey[k]).ToList();

    public int TotalUnread => byKey.Values.Sum(c => c.Unread);

    public Conversation Get(string key, string title)
    {
        if (!byKey.TryGetValue(key, out var c))
        {
            c = new Conversation(key, title);
            byKey[key] = c;
            order.Add(key);
        }

        return c;
    }

    public Conversation? Find(string key) => byKey.GetValueOrDefault(key);

    public bool Remove(string key)
    {
        order.Remove(key);
        return byKey.Remove(key);
    }
}
