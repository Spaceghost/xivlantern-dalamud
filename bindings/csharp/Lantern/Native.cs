// The raw C ABI. Nothing in this file allocates on the native side or keeps a
// pointer past the call that produced it; that discipline lives here so the
// rest of the binding can be ordinary C#.
//
// Loading follows the Ghostty shim's rule: resolve every export before
// publishing any, and free the library if one is missing, so a half-loaded
// native library never exists.

using System.Runtime.InteropServices;

namespace Lantern;

[StructLayout(LayoutKind.Sequential)]
internal struct LtConfig
{
    public uint AbiVersion;
    public nint DbPath;
    public nint BlobDir;
    public nint AppId;
    public uint RelayMode;
    public nint RelayUrl;
    public uint EventQueueCap;
    public uint AcceptInbound;
}

[StructLayout(LayoutKind.Sequential)]
internal unsafe struct LtEvent
{
    public uint Kind;
    public uint DataLen;
    public ulong Seq;
    public ulong Handle;
    public fixed byte Peer[32];
    public byte* Data;
}

[StructLayout(LayoutKind.Sequential)]
internal struct LtStats
{
    public ulong BytesSent;
    public ulong BytesRecv;
    public uint Rooms;
    public uint Peers;
    public uint DirectConns;
    public uint RelayedConns;
    public uint PublishingRooms;
    public uint EventsDropped;
    public uint RateLimited;
    public uint Reserved;
}

internal static unsafe class Native
{
    /// <summary>The ABI this binding was generated against.</summary>
    public const uint AbiVersion = 2;

    public const int NodeIdLen = 32;

    private static nint library;

    public static bool IsLoaded => library != 0;

    public static delegate* unmanaged[Cdecl]<uint> AbiVersionFn;
    public static delegate* unmanaged[Cdecl]<nint> BuildInfo;
    public static delegate* unmanaged[Cdecl]<int, nint> StatusText;
    public static delegate* unmanaged[Cdecl]<LtConfig*, nint*, int> NodeOpen;
    public static delegate* unmanaged[Cdecl]<nint, void> NodeClose;
    public static delegate* unmanaged[Cdecl]<nint, byte*, int> NodeId;
    public static delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int> NodeIdText;
    public static delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int> NodeTicket;
    public static delegate* unmanaged[Cdecl]<nint, LtEvent*, uint, int> Poll;
    public static delegate* unmanaged[Cdecl]<nint, LtStats*, int> StatsGet;
    public static delegate* unmanaged[Cdecl]<nint, ulong, int> IsPublishing;
    public static delegate* unmanaged[Cdecl]<nint, uint, byte*, byte*, ulong*, int> RoomJoin;
    public static delegate* unmanaged[Cdecl]<nint, ulong, int> RoomLeave;
    public static delegate* unmanaged[Cdecl]<nint, ulong, byte*, nuint, nuint*, int> RoomTicket;
    public static delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int> RoomSend;
    public static delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int> PresenceSet;
    public static delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, uint*, int> RoomPeers;
    public static delegate* unmanaged[Cdecl]<nint, byte*, ulong*, int> Connect;
    public static delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int> Send;
    public static delegate* unmanaged[Cdecl]<nint, ulong, int> Disconnect;
    public static delegate* unmanaged[Cdecl]<nint, byte*, byte*, ulong*, int> BlobAddFile;
    public static delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, ulong*, int> BlobAddBytes;
    public static delegate* unmanaged[Cdecl]<nint, ulong, ulong, int> BlobOffer;
    public static delegate* unmanaged[Cdecl]<nint, ulong, ulong, int> BlobUnoffer;
    public static delegate* unmanaged[Cdecl]<nint, ulong, byte*, int> BlobFetch;
    public static delegate* unmanaged[Cdecl]<nint, ulong, byte*, nuint, nuint*, int> BlobHashText;
    public static delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, byte*, int> BookmarkPut;
    public static delegate* unmanaged[Cdecl]<nint, byte*, int> BookmarkForget;
    public static delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int> BookmarkList;
    public static delegate* unmanaged[Cdecl]<nint, byte*, int> ProfileSetName;
    public static delegate* unmanaged[Cdecl]<nint, uint, byte*, int> PresenceStatusSet;
    public static delegate* unmanaged[Cdecl]<nint, uint, byte*, nuint, nuint*, int> InviteCreate;
    public static delegate* unmanaged[Cdecl]<nint, byte*, ulong*, int> InviteAccept;
    public static delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int> FriendList;
    public static delegate* unmanaged[Cdecl]<nint, byte*, byte*, uint, ulong*, int> FriendSend;
    public static delegate* unmanaged[Cdecl]<nint, byte*, int> FriendRemove;
    public static delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, nuint, nuint*, int> FriendHistory;
    public static delegate* unmanaged[Cdecl]<nint, byte*, ulong*, int> ChannelCreate;
    public static delegate* unmanaged[Cdecl]<nint, byte*, ulong, int> ChannelInvite;
    public static delegate* unmanaged[Cdecl]<nint, byte*, int> Block;
    public static delegate* unmanaged[Cdecl]<nint, byte*, int> Unblock;
    public static delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int> BlockedList;
    public static delegate* unmanaged[Cdecl]<nint, byte*, int> AddressHint;
    public static delegate* unmanaged[Cdecl]<nint, byte*, uint, int> SupportContactsSet;
    public static delegate* unmanaged[Cdecl]<nint, byte*, byte*, uint, ulong*, int> SupportSend;
    public static delegate* unmanaged[Cdecl]<nint, byte*, byte*, uint, ulong*, int> AuthorJoin;
    public static delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, nuint, nuint*, int> Announcements;
    public static delegate* unmanaged[Cdecl]<nint, ulong*, int> SelfTest;

    /// <summary>
    /// The file name to look for in the plugin folder. Dalamud loads the managed
    /// assembly from memory, so the caller must pass a real directory rather
    /// than relying on the working directory.
    /// </summary>
    public static string FileName =>
        OperatingSystem.IsWindows() ? "lantern.dll" : "liblantern.so";

    public static void Load(string directory)
    {
        if (library != 0)
        {
            return;
        }

        string path = Path.Combine(directory, FileName);
        nint lib = NativeLibrary.Load(path);
        try
        {
            AbiVersionFn = (delegate* unmanaged[Cdecl]<uint>)NativeLibrary.GetExport(lib, "lt_abi_version");
            uint abi = AbiVersionFn();
            if (abi != AbiVersion)
            {
                throw new LanternException(
                    $"{FileName} reports ABI {abi}, this binding speaks {AbiVersion}");
            }

            BuildInfo = (delegate* unmanaged[Cdecl]<nint>)NativeLibrary.GetExport(lib, "lt_build_info");
            StatusText = (delegate* unmanaged[Cdecl]<int, nint>)NativeLibrary.GetExport(lib, "lt_status_text");
            NodeOpen = (delegate* unmanaged[Cdecl]<LtConfig*, nint*, int>)NativeLibrary.GetExport(lib, "lt_node_open");
            NodeClose = (delegate* unmanaged[Cdecl]<nint, void>)NativeLibrary.GetExport(lib, "lt_node_close");
            NodeId = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lt_node_id");
            NodeIdText = (delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_node_id_text");
            NodeTicket = (delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_node_ticket");
            Poll = (delegate* unmanaged[Cdecl]<nint, LtEvent*, uint, int>)NativeLibrary.GetExport(lib, "lt_poll");
            StatsGet = (delegate* unmanaged[Cdecl]<nint, LtStats*, int>)NativeLibrary.GetExport(lib, "lt_stats_get");
            IsPublishing = (delegate* unmanaged[Cdecl]<nint, ulong, int>)NativeLibrary.GetExport(lib, "lt_is_publishing");
            RoomJoin = (delegate* unmanaged[Cdecl]<nint, uint, byte*, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lt_room_join");
            RoomLeave = (delegate* unmanaged[Cdecl]<nint, ulong, int>)NativeLibrary.GetExport(lib, "lt_room_leave");
            RoomTicket = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_room_ticket");
            RoomSend = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int>)NativeLibrary.GetExport(lib, "lt_room_send");
            PresenceSet = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int>)NativeLibrary.GetExport(lib, "lt_presence_set");
            RoomPeers = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, uint*, int>)NativeLibrary.GetExport(lib, "lt_room_peers");
            Connect = (delegate* unmanaged[Cdecl]<nint, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lt_connect");
            Send = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int>)NativeLibrary.GetExport(lib, "lt_send");
            Disconnect = (delegate* unmanaged[Cdecl]<nint, ulong, int>)NativeLibrary.GetExport(lib, "lt_disconnect");
            BlobAddFile = (delegate* unmanaged[Cdecl]<nint, byte*, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lt_blob_add_file");
            BlobAddBytes = (delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lt_blob_add_bytes");
            BlobOffer = (delegate* unmanaged[Cdecl]<nint, ulong, ulong, int>)NativeLibrary.GetExport(lib, "lt_blob_offer");
            BlobUnoffer = (delegate* unmanaged[Cdecl]<nint, ulong, ulong, int>)NativeLibrary.GetExport(lib, "lt_blob_unoffer");
            BlobFetch = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, int>)NativeLibrary.GetExport(lib, "lt_blob_fetch");
            BlobHashText = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_blob_hash_text");
            BookmarkPut = (delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, byte*, int>)NativeLibrary.GetExport(lib, "lt_bookmark_put");
            BookmarkForget = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lt_bookmark_forget");
            BookmarkList = (delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_bookmark_list");
            ProfileSetName = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lt_profile_set_name");
            PresenceStatusSet = (delegate* unmanaged[Cdecl]<nint, uint, byte*, int>)NativeLibrary.GetExport(lib, "lt_presence_status_set");
            InviteCreate = (delegate* unmanaged[Cdecl]<nint, uint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_invite_create");
            InviteAccept = (delegate* unmanaged[Cdecl]<nint, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lt_invite_accept");
            FriendList = (delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_friend_list");
            FriendSend = (delegate* unmanaged[Cdecl]<nint, byte*, byte*, uint, ulong*, int>)NativeLibrary.GetExport(lib, "lt_friend_send");
            FriendRemove = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lt_friend_remove");
            FriendHistory = (delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_friend_history");
            ChannelCreate = (delegate* unmanaged[Cdecl]<nint, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lt_channel_create");
            ChannelInvite = (delegate* unmanaged[Cdecl]<nint, byte*, ulong, int>)NativeLibrary.GetExport(lib, "lt_channel_invite");
            Block = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lt_block");
            Unblock = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lt_unblock");
            BlockedList = (delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_blocked_list");
            AddressHint = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lt_address_hint");
            SupportContactsSet = (delegate* unmanaged[Cdecl]<nint, byte*, uint, int>)NativeLibrary.GetExport(lib, "lt_support_contacts_set");
            SupportSend = (delegate* unmanaged[Cdecl]<nint, byte*, byte*, uint, ulong*, int>)NativeLibrary.GetExport(lib, "lt_support_send");
            AuthorJoin = (delegate* unmanaged[Cdecl]<nint, byte*, byte*, uint, ulong*, int>)NativeLibrary.GetExport(lib, "lt_author_join");
            Announcements = (delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lt_announcements");
            SelfTest = (delegate* unmanaged[Cdecl]<nint, ulong*, int>)NativeLibrary.GetExport(lib, "lt_selftest");

            library = lib;
        }
        catch
        {
            NativeLibrary.Free(lib);
            throw;
        }
    }

    public static void Unload()
    {
        if (library == 0)
        {
            return;
        }

        NativeLibrary.Free(library);
        library = 0;
    }

    public static string StatusMessage(int status)
    {
        if (StatusText == null)
        {
            return $"status {status}";
        }

        return Marshal.PtrToStringUTF8(StatusText(status)) ?? $"status {status}";
    }
}
