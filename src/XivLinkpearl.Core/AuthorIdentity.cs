namespace XivLinkpearl.Core;

/// <summary>
/// The mod author's public identity, embedded in the plugin. Only public halves live here: the author key that
/// signs announcements stays in one file on the author's own machine (<c>linkpearl author keygen</c>), and the
/// node ids are the author's always-on nodes, which bootstrap the author channel and take support messages.
/// See docs/AUTHOR.md. Empty means this build has no author channel yet, and the plugin says so.
/// </summary>
public static class AuthorIdentity
{
    /// <summary>ed25519 public key, 64 lowercase hex characters, from <c>linkpearl author pubkey</c>.</summary>
    public const string PublicKeyHex = "";

    /// <summary>NodeIds (64 hex) of the author's always-on nodes, from the ready line of <c>linkpearl --author-mode</c>.</summary>
    public static readonly string[] NodeIdHex = [];

    public static bool Configured => ParseKey(PublicKeyHex) is not null && Nodes.Count > 0;

    public static byte[]? Key => ParseKey(PublicKeyHex);

    public static IReadOnlyList<byte[]> Nodes => NodeIdHex.Select(ParseKey).OfType<byte[]>().ToList();

    /// <summary>32 bytes from 64 hex characters, or null.</summary>
    public static byte[]? ParseKey(string? hex)
    {
        if (hex is null || hex.Length != 64)
            return null;
        try
        {
            return Convert.FromHexString(hex);
        }
        catch (FormatException)
        {
            return null;
        }
    }
}
