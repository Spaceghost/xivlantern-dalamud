// The friendly half of the C# binding.
//
// Every method here except Open and Dispose is safe to call from the game's
// frame thread: they push work onto a queue in the native library and return.
// Results arrive from Poll(), which is expected to be called once per frame.

using System.Runtime.InteropServices;
using System.Text;

namespace Linkpearl;

public sealed class LinkpearlException : Exception
{
    public LinkpearlException(string message) : base(message)
    {
    }

    public LinkpearlException(string message, int status) : base($"{message}: {Native.StatusMessage(status)}")
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

public sealed class LinkpearlOptions
{
    /// <summary>Folder holding linkpearl.dll. In a Dalamud plugin this is
    /// <c>Pi.AssemblyLocation.DirectoryName</c>, never the working directory.</summary>
    public required string NativeDirectory { get; init; }

    /// <summary>SQLite file for the identity key and bookmarks. Null means a
    /// throwaway in-memory identity.</summary>
    public string? DatabasePath { get; init; }

    public string? BlobDirectory { get; init; }

    public string AppId { get; init; } = "ffxiv-linkpearl/1";

    public RelayMode Relay { get; init; } = RelayMode.Default;

    public string? RelayUrl { get; init; }

    public uint EventQueueCapacity { get; init; }

    /// <summary>Accept inbound connections. Off means strangers cannot reach
    /// this node at all.</summary>
    public bool AcceptInbound { get; init; } = true;
}

public sealed unsafe class LinkpearlNode : IDisposable
{
    private const int TextBufferSize = 4096;

    private nint handle;
    private readonly LpEvent[] eventBuffer = new LpEvent[64];

    private LinkpearlNode(nint handle)
    {
        this.handle = handle;
    }

    /// <summary>
    /// Blocking. Starts the runtime, opens SQLite and binds the endpoint.
    /// Call it from a background task, not from Draw or Framework.Update.
    /// </summary>
    public static LinkpearlNode Open(LinkpearlOptions options)
    {
        ArgumentNullException.ThrowIfNull(options);
        Native.Load(options.NativeDirectory);

        using var db = new Utf8Buffer(options.DatabasePath);
        using var blobs = new Utf8Buffer(options.BlobDirectory);
        using var appId = new Utf8Buffer(options.AppId);
        using var relayUrl = new Utf8Buffer(options.RelayUrl);

        var config = new LpConfig
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
            throw new LinkpearlException("could not open the node", status);
        }

        return new LinkpearlNode(node);
    }

    public static string BuildInfo
    {
        get
        {
            if (!Native.IsLoaded)
            {
                return "linkpearl (not loaded)";
            }

            return Marshal.PtrToStringUTF8(Native.BuildInfo()) ?? "linkpearl";
        }
    }

    public bool IsOpen => handle != 0;

    private nint Handle => handle != 0
        ? handle
        : throw new ObjectDisposedException(nameof(LinkpearlNode));

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
    public List<LinkpearlEvent> Poll(int max = 64)
    {
        var results = new List<LinkpearlEvent>();
        if (handle == 0)
        {
            return results;
        }

        int want = Math.Clamp(max, 1, eventBuffer.Length);
        fixed (LpEvent* buffer = eventBuffer)
        {
            int count = Native.Poll(handle, buffer, (uint)want);
            if (count < 0)
            {
                throw new LinkpearlException("poll failed", count);
            }

            for (int i = 0; i < count; i++)
            {
                LpEvent* e = buffer + i;
                var peer = new byte[Native.NodeIdLen];
                for (int b = 0; b < Native.NodeIdLen; b++)
                {
                    peer[b] = e->Peer[b];
                }

                byte[] data = e->DataLen == 0 || e->Data == null
                    ? []
                    : new ReadOnlySpan<byte>(e->Data, (int)e->DataLen).ToArray();

                results.Add(new LinkpearlEvent((LinkpearlEventKind)e->Kind, e->Seq, e->Handle, peer, data));
            }
        }

        return results;
    }

    public LinkpearlStats Stats()
    {
        LpStats stats;
        Check(Native.StatsGet(Handle, &stats), "stats");
        return new LinkpearlStats(
            stats.BytesSent,
            stats.BytesRecv,
            stats.Rooms,
            stats.Peers,
            stats.DirectConns,
            stats.RelayedConns,
            stats.PublishingRooms,
            stats.EventsDropped);
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

    /// <summary>The friend list as the JSON documented in linkpearl.h.</summary>
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

    public void ChannelInvite(byte[] peer, ulong room)
    {
        CheckPeer(peer);
        fixed (byte* p = peer)
        {
            Check(Native.ChannelInvite(Handle, p, room), "channel invite");
        }
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

        // -3 is LP_E_BUFFER: the only case where a bigger buffer helps.
        if (status == -3)
        {
            var heap = new byte[(int)needed + 1];
            fixed (byte* p = heap)
            {
                Check(call(p, (nuint)heap.Length, &needed), what);
                return Encoding.UTF8.GetString(p, (int)needed);
            }
        }

        throw new LinkpearlException(what, status);
    }

    private static void Check(int status, string what)
    {
        if (status != 0)
        {
            throw new LinkpearlException(what, status);
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
