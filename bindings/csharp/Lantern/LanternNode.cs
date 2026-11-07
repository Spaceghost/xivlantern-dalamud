// The friendly half of the C# binding.
//
// Every method here except Open, Dispose and Nostr* is safe to call from the game's
// frame thread: they push work onto a queue in the native library and return.
// Results arrive from Poll(), which is expected to be called once per frame.

using System.Runtime.InteropServices;
using System.Text;

namespace Lantern;

public sealed class LanternException : Exception
{
    public LanternException(string message) : base(message)
    {
    }

    public LanternException(string message, int status) : base($"{message}: {Native.StatusMessage(status)}")
    {
        Status = status;
    }

    public int Status { get; }
}

public enum RelayMode : uint
{
    /// <summary>Public relays plus hole punching. The default, and what works.</summary>
    Default = 0,

    /// <summary>Direct paths only. Nothing ever reaches a third party.</summary>
    Disabled = 1,

    /// <summary>A relay the player or the FC runs.</summary>
    Custom = 2,
}

public enum RoomScope : uint
{
    Party = 0,
    Fc = 1,
    Zone = 2,
    World = 3,
    Public = 4,
    Custom = 5,
}

public sealed class LanternOptions
{
    /// <summary>Folder holding lantern.dll. In a Dalamud plugin this is
    /// <c>Pi.AssemblyLocation.DirectoryName</c>, never the working directory.</summary>
    public required string NativeDirectory { get; init; }

    /// <summary>SQLite file for the identity key and bookmarks. Null means a
    /// throwaway in-memory identity.</summary>
    public string? DatabasePath { get; init; }

    public string? BlobDirectory { get; init; }

    public string AppId { get; init; } = "xivlantern/1";

    public RelayMode Relay { get; init; } = RelayMode.Default;

    public string? RelayUrl { get; init; }

    public uint EventQueueCapacity { get; init; }

    /// <summary>Accept inbound connections. Off means strangers cannot reach
    /// this node at all.</summary>
    public bool AcceptInbound { get; init; } = true;
}

public sealed unsafe class LanternNode : IDisposable
{
    private const int TextBufferSize = 4096;

    private nint handle;
    private readonly LtEvent[] eventBuffer = new LtEvent[64];

    private LanternNode(nint handle)
    {
        this.handle = handle;
    }

    /// <summary>
    /// Blocking. Starts the runtime, opens SQLite and binds the endpoint.
    /// Call it from a background task, not from Draw or Framework.Update.
    /// </summary>
    public static LanternNode Open(LanternOptions options)
    {
        ArgumentNullException.ThrowIfNull(options);
        Native.Load(options.NativeDirectory);

        using var db = new Utf8Buffer(options.DatabasePath);
        using var blobs = new Utf8Buffer(options.BlobDirectory);
        using var appId = new Utf8Buffer(options.AppId);
        using var relayUrl = new Utf8Buffer(options.RelayUrl);

        var config = new LtConfig
        {
            AbiVersion = Native.AbiVersion,
            DbPath = db.Pointer,
            BlobDir = blobs.Pointer,
            AppId = appId.Pointer,
            RelayMode = (uint)options.Relay,
            RelayUrl = relayUrl.Pointer,
            EventQueueCap = options.EventQueueCapacity,
            AcceptInbound = options.AcceptInbound ? 1u : 0u,
        };

        nint node;
        int status = Native.NodeOpen(&config, &node);
        if (status != 0)
        {
            throw new LanternException("could not open the node", status);
        }

        var result = new LanternNode(node);
        if (result.SupportsChannelHistory)
        {
            try { Check(Native.ChannelRecordEventsEnable(node, 1), "signed record events"); }
            catch { result.Dispose(); throw; }
        }
        return result;
    }

    public static string BuildInfo
    {
        get
        {
            if (!Native.IsLoaded)
            {
                return "lantern (not loaded)";
            }

            return Marshal.PtrToStringUTF8(Native.BuildInfo()) ?? "lantern";
        }
    }

    public bool IsOpen => handle != 0;

    private nint Handle => handle != 0
        ? handle
        : throw new ObjectDisposedException(nameof(LanternNode));

    public void Dispose()
    {
        nint h = Interlocked.Exchange(ref handle, 0);
        if (h != 0)
        {
            Native.NodeClose(h);
        }
    }

    // ------------------------------------------------------------- identity

    public byte[] NodeId()
    {
        var id = new byte[Native.NodeIdLen];
        fixed (byte* p = id)
        {
            Check(Native.NodeId(Handle, p), "node id");
        }

        return id;
    }

    public string NodeIdText() => Text((b, len, need) => Native.NodeIdText(Handle, b, len, need), "node id text");

    /// <summary>A ticket another player can paste to reach this node directly.</summary>
    public string NodeTicket() => Text((b, len, need) => Native.NodeTicket(Handle, b, len, need), "node ticket");

    // -------------------------------------------------------------- polling

    /// <summary>
    /// Drain pending events. Never blocks, allocates only for the events it
    /// actually returns, and copies every payload, so nothing here points into
    /// native memory once it returns.
    /// </summary>
    public List<LanternEvent> Poll(int max = 64)
    {
        var results = new List<LanternEvent>();
        if (handle == 0)
        {
            return results;
        }

        int want = Math.Clamp(max, 1, eventBuffer.Length);
        fixed (LtEvent* buffer = eventBuffer)
        {
            int count = Native.Poll(handle, buffer, (uint)want);
            if (count < 0)
            {
                throw new LanternException("poll failed", count);
            }

            for (int i = 0; i < count; i++)
            {
                LtEvent* e = buffer + i;
                var peer = new byte[Native.NodeIdLen];
                for (int b = 0; b < Native.NodeIdLen; b++)
                {
                    peer[b] = e->Peer[b];
                }

                byte[] data = e->DataLen == 0 || e->Data == null
                    ? []
                    : new ReadOnlySpan<byte>(e->Data, (int)e->DataLen).ToArray();

                results.Add(new LanternEvent((LanternEventKind)e->Kind, e->Seq, e->Handle, peer, data));
            }
        }

        return results;
    }

    public LanternStats Stats()
    {
        LtStats stats;
        Check(Native.StatsGet(Handle, &stats), "stats");
        return new LanternStats(
            stats.BytesSent,
            stats.BytesRecv,
            stats.Rooms,
            stats.Peers,
            stats.DirectConns,
            stats.RelayedConns,
            stats.PublishingRooms,
            stats.EventsDropped,
            stats.RateLimited);
    }

    /// <summary>Is this node serving anything into the room right now? The UI
    /// is expected to show this.</summary>
    public bool IsPublishing(ulong room) => Native.IsPublishing(Handle, room) == 1;

    // ---------------------------------------------------------------- rooms

    public ulong RoomJoin(RoomScope scope, string key, string? ticket = null)
    {
        using var keyBuf = new Utf8Buffer(key);
        using var ticketBuf = new Utf8Buffer(ticket);
        ulong room;
        Check(Native.RoomJoin(Handle, (uint)scope, (byte*)keyBuf.Pointer, (byte*)ticketBuf.Pointer, &room), "room join");
        return room;
    }

    public void RoomLeave(ulong room) => Check(Native.RoomLeave(Handle, room), "room leave");

    public string RoomTicket(ulong room) =>
        Text((b, len, need) => Native.RoomTicket(Handle, room, b, len, need), "room ticket");

    public void RoomSend(ulong room, ReadOnlySpan<byte> data)
    {
        fixed (byte* p = data)
        {
            Check(Native.RoomSend(Handle, room, p, (uint)data.Length), "room send");
        }
    }

    public void RoomSend(ulong room, string text) => RoomSend(room, Encoding.UTF8.GetBytes(text));

    public bool SupportsChannelHistory => Native.ChannelRecordEventsEnable != null && Native.RoomHistoryEnable != null && Native.RoomHistoryClear != null && Native.RoomSendWithHistory != null;

    /// <summary>Queue saved-history loading and automatic catch-up/serving; off by default.</summary>
    public void RoomHistoryEnable(ulong room, bool enabled)
    {
        if (!SupportsChannelHistory) throw new LanternException("Update Lantern to use channel history.");
        Check(Native.RoomHistoryEnable(Handle, room, enabled ? 1U : 0U), "channel history");
    }

    /// <summary>Clear local rows and cancel pending catch-up. Other members retain copies.</summary>
    public void RoomHistoryClear(ulong room)
    {
        if (!SupportsChannelHistory) throw new LanternException("Update Lantern to use channel history.");
        Check(Native.RoomHistoryClear(Handle, room), "clear channel history");
    }

    /// <summary>Queue signed room-bound text; record events carry its complete signature.</summary>
    public void RoomSendWithHistory(ulong room, ReadOnlySpan<byte> text)
    {
        if (!SupportsChannelHistory) throw new LanternException("Update Lantern to use channel history.");
        fixed (byte* p = text)
            Check(Native.RoomSendWithHistory(Handle, room, p, (uint)text.Length), "signed channel send");
    }

    public void RoomSendWithHistory(ulong room, string text) => RoomSendWithHistory(room, Encoding.UTF8.GetBytes(text));

    public void PresenceSet(ulong room, ReadOnlySpan<byte> data)
    {
        fixed (byte* p = data)
        {
            Check(Native.PresenceSet(Handle, room, p, (uint)data.Length), "presence");
        }
    }

    public void PresenceSet(ulong room, string text) => PresenceSet(room, Encoding.UTF8.GetBytes(text));

    public List<byte[]> RoomPeers(ulong room, int max = 64)
    {
        var flat = new byte[max * Native.NodeIdLen];
        uint count;
        fixed (byte* p = flat)
        {
            Check(Native.RoomPeers(Handle, room, p, (uint)max, &count), "room peers");
        }

        int take = Math.Min((int)count, max);
        var peers = new List<byte[]>(take);
        for (int i = 0; i < take; i++)
        {
            peers.Add(flat.AsSpan(i * Native.NodeIdLen, Native.NodeIdLen).ToArray());
        }

        return peers;
    }

    // --------------------------------------------------------------- direct

    public ulong Connect(string ticketOrNodeId)
    {
        using var buf = new Utf8Buffer(ticketOrNodeId);
        ulong conn;
        Check(Native.Connect(Handle, (byte*)buf.Pointer, &conn), "connect");
        return conn;
    }

    public void Send(ulong conn, ReadOnlySpan<byte> data)
    {
        fixed (byte* p = data)
        {
            Check(Native.Send(Handle, conn, p, (uint)data.Length), "send");
        }
    }

    public void Disconnect(ulong conn) => Check(Native.Disconnect(Handle, conn), "disconnect");

    // ---------------------------------------------------------------- blobs

    public ulong BlobAddFile(string path, string name)
    {
        using var pathBuf = new Utf8Buffer(path);
        using var nameBuf = new Utf8Buffer(name);
        ulong blob;
        Check(Native.BlobAddFile(Handle, (byte*)pathBuf.Pointer, (byte*)nameBuf.Pointer, &blob), "add file");
        return blob;
    }

    public ulong BlobAddBytes(ReadOnlySpan<byte> data, string name)
    {
        using var nameBuf = new Utf8Buffer(name);
        ulong blob;
        fixed (byte* p = data)
        {
            Check(Native.BlobAddBytes(Handle, p, (uint)data.Length, (byte*)nameBuf.Pointer, &blob), "add bytes");
        }

        return blob;
    }

    /// <summary>
    /// Announce a blob to a room. This is the only call that makes a file
    /// visible to anyone else, so it is the one a UI must gate behind a click.
    /// </summary>
    public void BlobOffer(ulong room, ulong blob) => Check(Native.BlobOffer(Handle, room, blob), "offer");

    public void BlobUnoffer(ulong room, ulong blob) => Check(Native.BlobUnoffer(Handle, room, blob), "unoffer");

    public void BlobFetch(ulong blob, string outPath)
    {
        using var buf = new Utf8Buffer(outPath);
        Check(Native.BlobFetch(Handle, blob, (byte*)buf.Pointer), "fetch");
    }

    public string BlobHash(ulong blob) =>
        Text((b, len, need) => Native.BlobHashText(Handle, blob, b, len, need), "blob hash");

    public bool SupportsBoundedFileFetch => Native.BlobFetchBounded != null && Native.BlobCancelBounded != null;

    /// <summary>Explicit bounded, hash-verified remote file fetch. Destination must
    /// be new and its parent privately controlled; no unsafe old-library fallback.</summary>
    public void BlobFetchBounded(ulong blob, string outPath)
    {
        if (!SupportsBoundedFileFetch) throw new NotSupportedException("Update Lantern to download shared files safely.");
        if (string.IsNullOrWhiteSpace(outPath) || outPath.Contains('\0')) throw new ArgumentException("Choose a valid download path.", nameof(outPath));
        using var buf = new Utf8Buffer(outPath);
        Check(Native.BlobFetchBounded(Handle, blob, (byte*)buf.Pointer), "bounded fetch");
    }

    public void BlobCancelBounded(ulong blob)
    {
        if (!SupportsBoundedFileFetch) throw new NotSupportedException("Update Lantern to manage shared file downloads safely.");
        Check(Native.BlobCancelBounded(Handle, blob), "cancel bounded fetch");
    }

    // -------------------------------------------------------------- friends

    /// <summary>The name friends see. Persisted.</summary>
    public void SetDisplayName(string name)
    {
        using var buf = new Utf8Buffer(name);
        Check(Native.ProfileSetName(Handle, (byte*)buf.Pointer), "name");
    }

    /// <summary>
    /// What friends see. The note must be something the player typed; never
    /// fill it from game state without the player asking.
    /// </summary>
    public void SetPresence(PresenceStatus status, string? note = null)
    {
        using var buf = new Utf8Buffer(note);
        Check(Native.PresenceStatusSet(Handle, (uint)status, (byte*)buf.Pointer), "presence status");
    }

    public bool SupportsActivityTags => Native.ActivityTagsSet != null;

    /// <summary>Player-supplied activities; an optional extension of ABI 2.</summary>
    public void SetActivityTags(IEnumerable<string> tags)
    {
        if (!SupportsActivityTags) throw new LanternException("Update Lantern to share activity tags.");
        using var buf = new Utf8Buffer(System.Text.Json.JsonSerializer.Serialize(tags));
        Check(Native.ActivityTagsSet(Handle, (byte*)buf.Pointer), "activity tags");
    }

    /// <summary>A single-use friend invite. 0 seconds never expires.</summary>
    public string InviteCreate(uint ttlSeconds = 0) =>
        Text((b, len, need) => Native.InviteCreate(Handle, ttlSeconds, b, len, need), "invite");

    /// <summary>Redeem an invite. FriendAdded or InviteFailed with the
    /// returned handle arrives from Poll().</summary>
    public ulong InviteAccept(string ticket)
    {
        using var buf = new Utf8Buffer(ticket);
        ulong handle;
        Check(Native.InviteAccept(Handle, (byte*)buf.Pointer, &handle), "invite accept");
        return handle;
    }

    /// <summary>The friend list as the JSON documented in lantern.h.</summary>
    public string FriendsJson() =>
        Text((b, len, need) => Native.FriendList(Handle, b, len, need), "friends");

    /// <summary>Send a 1:1 text; returns the id FriendDelivered will carry.</summary>
    public ulong FriendSend(byte[] peer, string text)
    {
        CheckPeer(peer);
        byte[] bytes = Encoding.UTF8.GetBytes(text);
        ulong id;
        fixed (byte* p = peer)
        fixed (byte* t = bytes)
        {
            Check(Native.FriendSend(Handle, p, t, (uint)bytes.Length, &id), "friend send");
        }

        return id;
    }

    public void FriendRemove(byte[] peer)
    {
        CheckPeer(peer);
        fixed (byte* p = peer)
        {
            Check(Native.FriendRemove(Handle, p), "friend remove");
        }
    }

    /// <summary>Last texts with a friend, oldest first, as JSON. Reads
    /// SQLite: keep it out of Draw.</summary>
    public string FriendHistoryJson(byte[] peer, uint limit = 50)
    {
        CheckPeer(peer);
        return Text(
            (b, len, need) =>
            {
                fixed (byte* p = peer)
                {
                    return Native.FriendHistory(Handle, p, limit, b, len, need);
                }
            },
            "friend history");
    }

    // ------------------------------------------------------------- channels

    /// <summary>A group channel nobody can find without its ticket.</summary>
    public ulong ChannelCreate(string label)
    {
        using var buf = new Utf8Buffer(label);
        ulong room;
        Check(Native.ChannelCreate(Handle, (byte*)buf.Pointer, &room), "channel create");
        return room;
    }

    public bool SupportsSealedChannels => SupportsBoundedFileFetch && Native.ChannelCreateSealed != null
        && Native.ChannelReseal != null && Native.RoomIsSealed != null && Native.BlobAddSealedBytes != null;

    public ulong ChannelCreateSealed(string label)
    {
        RequireSealedChannels();
        using var buf = new Utf8Buffer(label);
        ulong room;
        Check(Native.ChannelCreateSealed(Handle, (byte*)buf.Pointer, &room), "create sealed channel");
        return room;
    }

    public ulong ChannelReseal(ulong room)
    {
        RequireSealedChannels();
        ulong next;
        Check(Native.ChannelReseal(Handle, room, &next), "reseal channel");
        return next;
    }

    public bool RoomIsSealed(ulong room)
    {
        RequireSealedChannels();
        uint value;
        Check(Native.RoomIsSealed(Handle, room, &value), "channel sealing mode");
        return value == 1;
    }

    public ulong BlobAddSealedBytes(ulong room, ReadOnlySpan<byte> data, string name)
    {
        RequireSealedChannels();
        if (data.Length > 16 * 1024 * 1024) throw new ArgumentOutOfRangeException(nameof(data));
        using var nameBuf = new Utf8Buffer(name);
        ulong blob;
        fixed (byte* p = data)
            Check(Native.BlobAddSealedBytes(Handle, room, p, (uint)data.Length, (byte*)nameBuf.Pointer, &blob), "prepare sealed file");
        return blob;
    }

    private void RequireSealedChannels()
    {
        if (!SupportsSealedChannels) throw new NotSupportedException("Update Lantern to use sealed channels.");
    }

    public void ChannelInvite(byte[] peer, ulong room)
    {
        CheckPeer(peer);
        fixed (byte* p = peer)
        {
            Check(Native.ChannelInvite(Handle, p, room), "channel invite");
        }
    }

    // --------------------------------------------------------------- safety

    public bool SupportsFriendFiles => SupportsBoundedFileFetch && Native.FriendFileOffer != null && Native.FriendFileWithdraw != null;

    public void FriendFileOffer(byte[] peer, ulong blob)
    {
        CheckPeer(peer);
        if (!SupportsFriendFiles) throw new NotSupportedException("Update Lantern to share files with friends.");
        fixed (byte* p = peer) Check(Native.FriendFileOffer(Handle, p, blob), "friend file offer");
    }

    public void FriendFileWithdraw(byte[] peer, ulong blob)
    {
        CheckPeer(peer);
        if (!SupportsFriendFiles) throw new NotSupportedException("Update Lantern to manage shared files.");
        fixed (byte* p = peer) Check(Native.FriendFileWithdraw(Handle, p, blob), "friend file withdrawal");
    }

    /// <summary>Block a node: silently dropped from friends, and refused from then on. Persisted.</summary>
    public void Block(byte[] peer)
    {
        CheckPeer(peer);
        fixed (byte* p = peer)
        {
            Check(Native.Block(Handle, p), "block");
        }
    }

    /// <summary>False when the node was not blocked.</summary>
    public bool Unblock(byte[] peer)
    {
        CheckPeer(peer);
        int status;
        fixed (byte* p = peer)
        {
            status = Native.Unblock(Handle, p);
        }

        if (status == -4)
        {
            return false;
        }

        Check(status, "unblock");
        return true;
    }

    /// <summary>JSON array of hex node ids.</summary>
    public string BlockedJson() =>
        Text((b, len, need) => Native.BlockedList(Handle, b, len, need), "blocked");

    /// <summary>Remember where a node is (an ltnode… ticket), for relay-off use.</summary>
    public void AddressHint(string nodeTicket)
    {
        using var buf = new Utf8Buffer(nodeTicket);
        Check(Native.AddressHint(Handle, (byte*)buf.Pointer), "address hint");
    }

    // ------------------------------------------------------ author channel

    /// <summary>Join the author channel of an ed25519 author key, bootstrapped by the author's nodes.</summary>
    public ulong AuthorJoin(byte[] authorKey, IReadOnlyList<byte[]> bootstrap)
    {
        CheckPeer(authorKey);
        byte[] flat = Flatten(bootstrap);
        ulong room;
        fixed (byte* a = authorKey)
        fixed (byte* b = flat)
        {
            Check(Native.AuthorJoin(Handle, a, flat.Length == 0 ? null : b, (uint)bootstrap.Count, &room), "author join");
        }

        return room;
    }

    /// <summary>Kept announcements, newest first, as JSON. Reads SQLite: keep it out of Draw.</summary>
    public string AnnouncementsJson(byte[] authorKey, uint limit = 20)
    {
        CheckPeer(authorKey);
        return Text(
            (b, len, need) =>
            {
                fixed (byte* a = authorKey)
                {
                    return Native.Announcements(Handle, a, limit, b, len, need);
                }
            },
            "announcements");
    }

    public void SetSupportContacts(IReadOnlyList<byte[]> peers)
    {
        byte[] flat = Flatten(peers);
        fixed (byte* p = flat)
        {
            Check(Native.SupportContactsSet(Handle, flat.Length == 0 ? null : p, (uint)peers.Count), "support contacts");
        }
    }

    /// <summary>Write to the author. SupportDelivered or SupportFailed follows; nothing is retried.</summary>
    public ulong SupportSend(byte[] peer, string text)
    {
        CheckPeer(peer);
        byte[] bytes = Encoding.UTF8.GetBytes(text);
        ulong id;
        fixed (byte* p = peer)
        fixed (byte* t = bytes)
        {
            Check(Native.SupportSend(Handle, p, t, (uint)bytes.Length, &id), "support send");
        }

        return id;
    }

    // ------------------------------------------------------------ selftest

    /// <summary>Start a reachability test; a SelfTest event with this handle carries the JSON report.</summary>
    public ulong SelfTest()
    {
        ulong handle;
        Check(Native.SelfTest(Handle, &handle), "selftest");
        return handle;
    }

    /// <summary>Whether this library includes all optional name-card invite exports.</summary>
    public bool SupportsNostrInvites => Native.NostrIdentity != null
        && Native.NostrSendInvite != null && Native.NostrCheckInbox != null;

    private void RequireNostrInvites()
    {
        if (!SupportsNostrInvites)
            throw new LanternException("This Lantern library does not include name-card invites.");
    }

    /// <summary>Create/load the local identity and return its public npub. Background thread only;
    /// may touch SQLite/cryptography. No network. Keep the node alive until this call returns.</summary>
    public string NostrIdentity()
    {
        RequireNostrInvites();
        return Text((b, len, need) => Native.NostrIdentity(Handle, b, len, need), "name-card identity");
    }

    /// <summary>Queue an invite to an npub/nprofile/hex key. Background thread only; preparation
    /// touches SQLite/cryptography. Explicit relays; completion arrives via NostrDone/NostrFailed.
    /// Keep the node alive until this call returns.</summary>
    public ulong NostrSendInvite(IReadOnlyList<string> relays, string recipient, uint ttlSeconds = 86400)
    {
        RequireNostrInvites();
        ArgumentNullException.ThrowIfNull(recipient);
        if (recipient.Length == 0 || Encoding.UTF8.GetByteCount(recipient) > 1024
            || recipient.Any(char.IsControl)) throw new ArgumentException("Invalid name-card recipient.", nameof(recipient));
        if (ttlSeconds is 0 or > 604800) throw new ArgumentOutOfRangeException(nameof(ttlSeconds));
        var encoded = EncodeNostrRelays(relays);
        using var to = new Utf8Buffer(recipient);
        ulong handle;
        fixed (byte* p = encoded)
            Check(Native.NostrSendInvite(Handle, p, (uint)encoded.Length, (byte*)to.Pointer, ttlSeconds, &handle), "name-card invite");
        return handle;
    }

    /// <summary>Queue an inbox check on explicitly chosen relays. Background thread only;
    /// incoming NostrInvite events require explicit acceptance. Keep the node alive until return.</summary>
    public ulong NostrCheckInbox(IReadOnlyList<string> relays)
    {
        RequireNostrInvites();
        var encoded = EncodeNostrRelays(relays);
        ulong handle;
        fixed (byte* p = encoded)
            Check(Native.NostrCheckInbox(Handle, p, (uint)encoded.Length, &handle), "name-card inbox");
        return handle;
    }

    private static byte[] EncodeNostrRelays(IReadOnlyList<string> relays)
    {
        ArgumentNullException.ThrowIfNull(relays);
        if (relays.Count is < 1 or > 8) throw new ArgumentException("Choose one to eight relays.", nameof(relays));
        foreach (var url in relays)
            if (string.IsNullOrEmpty(url) || Encoding.UTF8.GetByteCount(url) > 2048
                || url.Any(c => char.IsControl(c) || char.IsWhiteSpace(c))
                || !Uri.TryCreate(url, UriKind.Absolute, out var uri) || string.IsNullOrEmpty(uri.Host)
                || uri.Scheme is not ("ws" or "wss"))
                throw new ArgumentException("Invalid WebSocket relay URL.", nameof(relays));
        var encoded = Encoding.UTF8.GetBytes(string.Join('\n', relays));
        if (encoded.Length > 16384) throw new ArgumentException("Relay list is too long.", nameof(relays));
        return encoded;
    }

    private static byte[] Flatten(IReadOnlyList<byte[]> peers)
    {
        var flat = new byte[peers.Count * Native.NodeIdLen];
        for (int i = 0; i < peers.Count; i++)
        {
            CheckPeer(peers[i]);
            peers[i].CopyTo(flat, i * Native.NodeIdLen);
        }

        return flat;
    }

    private static void CheckPeer(byte[] peer)
    {
        ArgumentNullException.ThrowIfNull(peer);
        if (peer.Length != Native.NodeIdLen)
        {
            throw new ArgumentException($"a node id is {Native.NodeIdLen} bytes", nameof(peer));
        }
    }

    // ------------------------------------------------------------ bookmarks

    public void BookmarkPut(string id, RoomScope scope, string title, string ticket)
    {
        using var idBuf = new Utf8Buffer(id);
        using var titleBuf = new Utf8Buffer(title);
        using var ticketBuf = new Utf8Buffer(ticket);
        Check(
            Native.BookmarkPut(Handle, (byte*)idBuf.Pointer, (uint)scope, (byte*)titleBuf.Pointer, (byte*)ticketBuf.Pointer),
            "bookmark");
    }

    public bool BookmarkForget(string id)
    {
        using var buf = new Utf8Buffer(id);
        int status = Native.BookmarkForget(Handle, (byte*)buf.Pointer);
        if (status == -4)
        {
            return false;
        }

        Check(status, "bookmark forget");
        return true;
    }

    public string BookmarksJson() =>
        Text((b, len, need) => Native.BookmarkList(Handle, b, len, need), "bookmarks");

    // -------------------------------------------------------------- helpers

    private delegate int TextCall(byte* buffer, nuint bufferLen, nuint* needed);

    private static string Text(TextCall call, string what)
    {
        Span<byte> buffer = stackalloc byte[TextBufferSize];
        nuint needed;
        int status;
        fixed (byte* p = buffer)
        {
            status = call(p, (nuint)buffer.Length, &needed);
            if (status == 0)
            {
                return Encoding.UTF8.GetString(p, (int)needed);
            }
        }

        // -3 is LT_E_BUFFER: the only case where a bigger buffer helps.
        if (status == -3)
        {
            var heap = new byte[(int)needed + 1];
            fixed (byte* p = heap)
            {
                Check(call(p, (nuint)heap.Length, &needed), what);
                return Encoding.UTF8.GetString(p, (int)needed);
            }
        }

        throw new LanternException(what, status);
    }

    private static void Check(int status, string what)
    {
        if (status != 0)
        {
            throw new LanternException(what, status);
        }
    }

    /// <summary>A NUL-terminated UTF-8 copy of a string, freed on dispose. Null
    /// in, null pointer out, which is what the C ABI reads as "not given".</summary>
    private readonly struct Utf8Buffer : IDisposable
    {
        public Utf8Buffer(string? value)
        {
            Pointer = value is null ? 0 : Marshal.StringToCoTaskMemUTF8(value);
        }

        public nint Pointer { get; }

        public void Dispose()
        {
            if (Pointer != 0)
            {
                Marshal.FreeCoTaskMem(Pointer);
            }
        }
    }
}
