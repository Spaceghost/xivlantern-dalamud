namespace XivLinkpearl.Shared;

/// <summary>
/// The Dalamud IPC gates XivLinkpearl publishes for sibling plugins (see docs/IPC.md). Everything that reads the
/// player's friends or messages, or sends as them, is refused until the player allows it in settings, and every
/// gate answers "error: ..." rather than throwing.
/// </summary>
public static class IpcContract
{
    /// <summary>nothing → string JSON <c>{"enabled","online","node_id","name","friends_online","unread"}</c>. Always answers.</summary>
    public const string GetStatus = "Linkpearl.v1.GetStatus";

    /// <summary>nothing → string JSON <c>[{"id","name","online","status","note"}]</c>, or "error: ..." unless reading is allowed.</summary>
    public const string GetFriends = "Linkpearl.v1.GetFriends";

    /// <summary>(friend id or name, text) → "ok: &lt;message id&gt;" or "error: ...". Needs "let other plugins send".</summary>
    public const string SendToFriend = "Linkpearl.v1.SendToFriend";

    /// <summary>friend id or name (or "") → nothing. Opens the Linkpearl window on that chat. Always allowed.</summary>
    public const string OpenChat = "Linkpearl.v1.OpenChat";

    /// <summary>
    /// Event, string JSON <c>{"kind","from","name","text"}</c> for each new friend text, author reply or announcement.
    /// Only sent while reading is allowed.
    /// </summary>
    public const string MessageReceived = "Linkpearl.v1.MessageReceived";
}
