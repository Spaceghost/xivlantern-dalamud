using System.Security.Cryptography;
using System.Text;

namespace XivLantern.Core;

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
public sealed record ChatLine(DateTimeOffset Time, string From, string Text, bool Outgoing, ulong Id = 0, LineState State = LineState.None, bool System = false, bool Verified = false, string? HistorySignature = null, long? ClaimedAt = null, string? SignedAuthor = null);

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
        // Native verification supplies the full signature; repeated text has
        // distinct signatures. Never infer channel identity from text or time.
        if (line.HistorySignature is { } signature && lines.Any(l => l.HistorySignature == signature))
            return;
        // A friend text is raised once, but history loaded after the fact may already hold it.
        if (line.HistorySignature is null && line.Id != 0 && !line.Outgoing && !line.System && lines.Any(l => l.Id == line.Id && !l.Outgoing))
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

    public void Clear()
    {
        lines.Clear();
        Unread = 0;
        Version++;
    }

    /// <summary>Replace the lines with history from SQLite (oldest first), keeping anything newer already here.</summary>
    public void LoadHistory(IEnumerable<ChatLine> history)
    {
        var signatures = new HashSet<string>(StringComparer.Ordinal);
        var older = history.Where(l => l.HistorySignature is null || signatures.Add(l.HistorySignature)).ToList();
        var ids = older.Where(l => l.Id != 0 && l.HistorySignature is null).Select(l => (l.Id, l.Outgoing)).ToHashSet();
        var keep = lines.Where(l => l.HistorySignature is { } signature
            ? !signatures.Contains(signature)
            : l.Id == 0 || !ids.Contains((l.Id, l.Outgoing))).ToList();
        var merged = older.Concat(keep);
        if (older.Any(l => l.HistorySignature is not null) || keep.Any(l => l.HistorySignature is not null))
            merged = merged.OrderBy(l => l.ClaimedAt ?? l.Time.ToUnixTimeMilliseconds()).ThenBy(l => l.HistorySignature, StringComparer.Ordinal);
        lines.Clear();
        lines.AddRange(merged);
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
    /// <summary>Stable across sessions. Sealed conversation IDs never carry the bearer ticket/key.</summary>
    public static string ChannelKey(string ticket)
    {
        var canonical = ticket.Trim().ToLowerInvariant();
        return canonical.StartsWith("ltsealed", StringComparison.Ordinal)
            ? "c:sealed:" + Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(canonical)))
            : "c:" + ticket;
    }

    /// <summary>Migrate earlier sealed mute IDs, retaining ordinary/friend/author settings.</summary>
    public static HashSet<string> NormalizeConversationKeys(IEnumerable<string>? keys) =>
        (keys ?? []).Where(k => !string.IsNullOrEmpty(k)).Select(k =>
            k.StartsWith("c:ltsealed", StringComparison.OrdinalIgnoreCase) ? ChannelKey(k[2..]) : k)
            .ToHashSet(StringComparer.Ordinal);

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
