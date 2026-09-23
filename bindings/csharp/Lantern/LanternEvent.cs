// Managed shapes for what comes out of Poll(). Everything is a copy: nothing
// here points into native memory.

using System.Text;

namespace Lantern;

public enum LanternEventKind : uint
{
    Ready = 1,
    RoomJoined = 2,
    RoomLeft = 3,
    PeerJoined = 4,
    PeerLeft = 5,
    Presence = 6,
    Message = 7,
    Direct = 8,
    Connected = 9,
    Disconnected = 10,
    BlobOffer = 11,
    BlobProgress = 12,
    BlobDone = 13,
    BlobFailed = 14,
    Ticket = 15,
    Publishing = 16,
    Error = 17,
    Log = 18,

    /// <summary>Handle = invite handle (0 when they redeemed ours); Text = name.</summary>
    FriendAdded = 19,
    FriendRemoved = 20,
    /// <summary>Handle = invite handle; Text = reason.</summary>
    InviteFailed = 21,
    /// <summary>Handle = <see cref="PresenceStatus"/>; Text = the friend's note.</summary>
    FriendOnline = 22,
    FriendOffline = 23,
    /// <summary>Handle = message id; Text = the message.</summary>
    FriendText = 24,
    /// <summary>Handle = the id <see cref="LanternNode.FriendSend"/> returned.</summary>
    FriendDelivered = 25,
    /// <summary>Text = a room ticket for <see cref="LanternNode.RoomJoin"/>.</summary>
    ChannelInvite = 26,
    /// <summary>A peer's frames are being dropped; Text = "friend", "room" or "stranger".</summary>
    RateLimited = 27,
    /// <summary>A verified author announcement: Handle = room, Peer = author key, Text = JSON.</summary>
    Announcement = 28,
    /// <summary>Author mode only.</summary>
    SupportMessage = 29,
    /// <summary>The author answered: Handle = id, Text = the answer.</summary>
    SupportReply = 30,
    SupportDelivered = 31,
    /// <summary>Handle = id, Text = reason. Not retried.</summary>
    SupportFailed = 32,
    /// <summary>Handle = the SelfTest() handle; Text = JSON report.</summary>
    SelfTest = 33,
}

public enum PresenceStatus : uint
{
    Invisible = 0,
    Online = 1,
    Away = 2,
    Busy = 3,
}

/// <param name="Kind">What happened.</param>
/// <param name="Seq">Monotonic per node, so a UI can dedupe or order.</param>
/// <param name="Handle">Room, connection or blob, depending on Kind.</param>
/// <param name="Peer">The other node's id, or 32 zero bytes.</param>
/// <param name="Data">Payload, already copied.</param>
public readonly record struct LanternEvent(
    LanternEventKind Kind,
    ulong Seq,
    ulong Handle,
    byte[] Peer,
    byte[] Data)
{
    public string Text => Data.Length == 0 ? string.Empty : Encoding.UTF8.GetString(Data);

    public string PeerHex => Convert.ToHexStringLower(Peer);

    /// <summary>True for <see cref="LanternEventKind.Publishing"/> events that
    /// mean "you are live".</summary>
    public bool Publishing => Kind == LanternEventKind.Publishing && Data.Length > 0 && Data[0] != 0;

    /// <summary>For <see cref="LanternEventKind.FriendOnline"/>.</summary>
    public PresenceStatus Status => (PresenceStatus)Handle;

    /// <summary>Bytes transferred so far, for a progress event.</summary>
    public (ulong Done, ulong Total) Progress => Data.Length >= 16
        ? (BitConverter.ToUInt64(Data, 0), BitConverter.ToUInt64(Data, 8))
        : (0UL, 0UL);
}

public readonly record struct LanternStats(
    ulong BytesSent,
    ulong BytesRecv,
    uint Rooms,
    uint Peers,
    uint DirectConnections,
    uint RelayedConnections,
    uint PublishingRooms,
    uint EventsDropped,
    uint RateLimited);
