using Dalamud.Configuration;
using XivLinkpearl.Core;

namespace XivLinkpearl.Plugin;

public sealed class SavedChannel
{
    public string Label { get; set; } = "";
    public string Ticket { get; set; } = "";
}

public sealed class Configuration : IPluginConfiguration
{
    public int Version { get; set; } = 1;

    /// <summary>Nothing connects, and the native library is not even loaded, until the player turns this on.</summary>
    public bool Enabled { get; set; }

    /// <summary>The privacy note was shown and ticked. Turning on is refused until it is.</summary>
    public bool PrivacyAcknowledged { get; set; }

    public RelayChoice Relay { get; set; } = RelayChoice.Default;
    public string CustomRelayUrl { get; set; } = "";

    /// <summary>What friends see. Typed by the player; never filled from the game.</summary>
    public string DisplayName { get; set; } = "";
    public uint Presence { get; set; } = 1;
    public string PresenceNote { get; set; } = "";

    public bool Toasts { get; set; } = true;
    public bool ChannelToasts { get; set; }
    public bool FriendOnlineToasts { get; set; }
    public bool ShowDtr { get; set; } = true;

    /// <summary>Opt in to the author's announcement channel (and the option to write to the author).</summary>
    public bool JoinAuthorChannel { get; set; }

    public bool RejoinChannels { get; set; } = true;
    public List<SavedChannel> Channels { get; set; } = [];

    /// <summary>Conversation keys ("f:&lt;id&gt;", "c:&lt;label&gt;", "author") that never notify or count as unread.</summary>
    public HashSet<string> Muted { get; set; } = [];

    /// <summary>Let other plugins read friends and incoming messages over IPC.</summary>
    public bool AllowIpcRead { get; set; }

    /// <summary>Let other plugins send 1:1 messages as the player over IPC.</summary>
    public bool AllowIpcSend { get; set; }
}
