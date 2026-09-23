namespace XivLantern.Core;

/// <summary>LT_RELAY_*.</summary>
public enum RelayChoice : uint
{
    /// <summary>n0's public relays plus hole punching.</summary>
    Default = 0,
    /// <summary>Direct paths only (LAN, open ports). No third party.</summary>
    Off = 1,
    /// <summary>A relay the player or their FC runs.</summary>
    Custom = 2,
}

/// <summary>The words the plugin shows about privacy. Kept here so they are reviewed and tested in one place.</summary>
public static class Privacy
{
    public const string Summary =
        "Lantern is peer to peer: your game connects straight to your friends' games, and nothing goes through a " +
        "Lantern server. Nothing from the game is ever sent — not your character's name, world, location or chat. " +
        "The name your friends see is whatever you type here.";

    public const string WhoSeesWhat =
        "Friends you add see your node id, the name and status you set, what you send them, and the IP addresses your " +
        "game can be reached on. Anyone holding a channel's ticket can join it, see your node id and read it. " +
        "Joining the author channel shows your node id and IP address to the other members you connect through.";

    public static string Relay(RelayChoice choice, string customUrl) => choice switch
    {
        RelayChoice.Default =>
            "Relays: n0's public relays (run by the iroh developers). Your game keeps a connection to one, so it sees your " +
            "IP address and node id and when you are online; when no direct path can be made it forwards your encrypted " +
            "traffic. It cannot read messages.",
        RelayChoice.Off =>
            "Relays: off. No third party is contacted at all, including n0's address lookup. Only direct paths work — on " +
            "your LAN, or with open ports — so most friends elsewhere will not reach you.",
        RelayChoice.Custom =>
            $"Relays: your own relay at {(string.IsNullOrWhiteSpace(customUrl) ? "(no URL set)" : customUrl)}. Its operator " +
            "sees your IP address and node id. n0's address lookup is still used to find friends by node id.",
        _ => "",
    };

    public const string Storage =
        "Your node key, friend list, 1:1 messages and settings are kept in this plugin's config folder, unencrypted, like " +
        "an SSH key. Anyone with that folder can act as you on Lantern.";

    public const string TermsOfService =
        "Third-party plugins are against the FINAL FANTASY XIV terms of service. Lantern never automates or sends a game " +
        "action; it is a separate chat that draws inside the game.";
}
