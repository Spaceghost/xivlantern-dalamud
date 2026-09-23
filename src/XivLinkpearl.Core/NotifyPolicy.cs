namespace XivLinkpearl.Core;

public enum Incoming
{
    FriendText,
    ChannelLine,
    Announcement,
    SupportReply,
    FriendOnline,
    InviteAccepted,
}

/// <summary>The player's notification settings at the moment something arrives.</summary>
public sealed record NotifySettings(bool Enabled, bool Toasts, bool ChannelToasts, bool FriendOnlineToasts, IReadOnlySet<string> Muted);

/// <summary>Decides whether an incoming thing becomes a toast. Nothing ever does while Linkpearl is off.</summary>
public static class NotifyPolicy
{
    public static bool Toast(Incoming what, string conversation, NotifySettings s, string? openConversation)
    {
        if (!s.Enabled || !s.Toasts || s.Muted.Contains(conversation))
            return false;
        // Already looking at it.
        if (openConversation == conversation && what is Incoming.FriendText or Incoming.ChannelLine or Incoming.SupportReply)
            return false;
        return what switch
        {
            Incoming.ChannelLine => s.ChannelToasts,
            Incoming.FriendOnline => s.FriendOnlineToasts,
            _ => true,
        };
    }

    /// <summary>Whether it counts towards the unread badge: muted conversations never do.</summary>
    public static bool CountsUnread(string conversation, NotifySettings s, string? openConversation) =>
        !s.Muted.Contains(conversation) && openConversation != conversation;
}
