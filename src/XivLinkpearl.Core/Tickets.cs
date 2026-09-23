namespace XivLinkpearl.Core;

public enum TicketKind
{
    None,
    FriendInvite,
    Room,
    Node,
    Announcement,
}

/// <summary>Recognises the tickets a player pastes. Never acts on them: accepting is always a separate click.</summary>
public static class Tickets
{
    private static readonly (string Prefix, TicketKind Kind)[] Prefixes =
    [
        ("lpfriend", TicketKind.FriendInvite),
        ("lproom", TicketKind.Room),
        ("lpnode", TicketKind.Node),
        ("lpannounce", TicketKind.Announcement),
    ];

    /// <summary>
    /// The first ticket anywhere in <paramref name="pasted"/> (a copied chat line often carries more than the
    /// ticket), or (None, "").
    /// </summary>
    public static (TicketKind Kind, string Ticket) Find(string? pasted)
    {
        if (string.IsNullOrWhiteSpace(pasted))
            return (TicketKind.None, "");
        foreach (var word in pasted.Split([' ', '\t', '\r', '\n', '"', '\'', '<', '>', '(', ')', ',', ';'], StringSplitOptions.RemoveEmptyEntries))
        {
            var w = word.Trim().TrimEnd('.').ToLowerInvariant();
            foreach (var (prefix, kind) in Prefixes)
            {
                if (w.Length > prefix.Length + 8 && w.StartsWith(prefix, StringComparison.Ordinal) && IsBase32(w.AsSpan(prefix.Length)))
                    return (kind, w);
            }
        }

        return (TicketKind.None, "");
    }

    private static bool IsBase32(ReadOnlySpan<char> s)
    {
        foreach (char c in s)
        {
            if (!(c is >= 'a' and <= 'z' || c is >= '2' and <= '7'))
                return false;
        }

        return true;
    }

    public static string Describe(TicketKind kind) => kind switch
    {
        TicketKind.FriendInvite => "a friend invite",
        TicketKind.Room => "a channel ticket",
        TicketKind.Node => "a node address",
        TicketKind.Announcement => "a signed announcement",
        _ => "not a Linkpearl ticket",
    };
}
