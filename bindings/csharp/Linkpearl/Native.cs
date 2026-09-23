// The raw C ABI. Nothing in this file allocates on the native side or keeps a
// pointer past the call that produced it; that discipline lives here so the
// rest of the binding can be ordinary C#.
//
// Loading follows the Ghostty shim's rule: resolve every export before
// publishing any, and free the library if one is missing, so a half-loaded
// native library never exists.

using System.Runtime.InteropServices;

namespace Linkpearl;

[StructLayout(LayoutKind.Sequential)]
internal struct LpConfig
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
internal unsafe struct LpEvent
{
    public uint Kind;
    public uint DataLen;
    public ulong Seq;
    public ulong Handle;
    public fixed byte Peer[32];
    public byte* Data;
}

[StructLayout(LayoutKind.Sequential)]
internal struct LpStats
{
    public ulong BytesSent;
    public ulong BytesRecv;
    public uint Rooms;
    public uint Peers;
    public uint DirectConns;
    public uint RelayedConns;
    public uint PublishingRooms;
    public uint EventsDropped;
}

internal static unsafe class Native
{
    /// <summary>The ABI this binding was generated against.</summary>
    public const uint AbiVersion = 1;

    public const int NodeIdLen = 32;

    private static nint library;

    public static bool IsLoaded => library != 0;

    public static delegate* unmanaged[Cdecl]<uint> AbiVersionFn;
    public static delegate* unmanaged[Cdecl]<nint> BuildInfo;
    public static delegate* unmanaged[Cdecl]<int, nint> StatusText;
    public static delegate* unmanaged[Cdecl]<LpConfig*, nint*, int> NodeOpen;
    public static delegate* unmanaged[Cdecl]<nint, void> NodeClose;
    public static delegate* unmanaged[Cdecl]<nint, byte*, int> NodeId;
    public static delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int> NodeIdText;
    public static delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int> NodeTicket;
    public static delegate* unmanaged[Cdecl]<nint, LpEvent*, uint, int> Poll;
    public static delegate* unmanaged[Cdecl]<nint, LpStats*, int> StatsGet;
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

    /// <summary>
    /// The file name to look for in the plugin folder. Dalamud loads the managed
    /// assembly from memory, so the caller must pass a real directory rather
    /// than relying on the working directory.
    /// </summary>
    public static string FileName =>
        OperatingSystem.IsWindows() ? "linkpearl.dll" : "liblinkpearl.so";

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
            AbiVersionFn = (delegate* unmanaged[Cdecl]<uint>)NativeLibrary.GetExport(lib, "lp_abi_version");
            uint abi = AbiVersionFn();
            if (abi != AbiVersion)
            {
                throw new LinkpearlException(
                    $"{FileName} reports ABI {abi}, this binding speaks {AbiVersion}");
            }

            BuildInfo = (delegate* unmanaged[Cdecl]<nint>)NativeLibrary.GetExport(lib, "lp_build_info");
            StatusText = (delegate* unmanaged[Cdecl]<int, nint>)NativeLibrary.GetExport(lib, "lp_status_text");
            NodeOpen = (delegate* unmanaged[Cdecl]<LpConfig*, nint*, int>)NativeLibrary.GetExport(lib, "lp_node_open");
            NodeClose = (delegate* unmanaged[Cdecl]<nint, void>)NativeLibrary.GetExport(lib, "lp_node_close");
            NodeId = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lp_node_id");
            NodeIdText = (delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lp_node_id_text");
            NodeTicket = (delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lp_node_ticket");
            Poll = (delegate* unmanaged[Cdecl]<nint, LpEvent*, uint, int>)NativeLibrary.GetExport(lib, "lp_poll");
            StatsGet = (delegate* unmanaged[Cdecl]<nint, LpStats*, int>)NativeLibrary.GetExport(lib, "lp_stats_get");
            IsPublishing = (delegate* unmanaged[Cdecl]<nint, ulong, int>)NativeLibrary.GetExport(lib, "lp_is_publishing");
            RoomJoin = (delegate* unmanaged[Cdecl]<nint, uint, byte*, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lp_room_join");
            RoomLeave = (delegate* unmanaged[Cdecl]<nint, ulong, int>)NativeLibrary.GetExport(lib, "lp_room_leave");
            RoomTicket = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lp_room_ticket");
            RoomSend = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int>)NativeLibrary.GetExport(lib, "lp_room_send");
            PresenceSet = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int>)NativeLibrary.GetExport(lib, "lp_presence_set");
            RoomPeers = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, uint*, int>)NativeLibrary.GetExport(lib, "lp_room_peers");
            Connect = (delegate* unmanaged[Cdecl]<nint, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lp_connect");
            Send = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, uint, int>)NativeLibrary.GetExport(lib, "lp_send");
            Disconnect = (delegate* unmanaged[Cdecl]<nint, ulong, int>)NativeLibrary.GetExport(lib, "lp_disconnect");
            BlobAddFile = (delegate* unmanaged[Cdecl]<nint, byte*, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lp_blob_add_file");
            BlobAddBytes = (delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, ulong*, int>)NativeLibrary.GetExport(lib, "lp_blob_add_bytes");
            BlobOffer = (delegate* unmanaged[Cdecl]<nint, ulong, ulong, int>)NativeLibrary.GetExport(lib, "lp_blob_offer");
            BlobUnoffer = (delegate* unmanaged[Cdecl]<nint, ulong, ulong, int>)NativeLibrary.GetExport(lib, "lp_blob_unoffer");
            BlobFetch = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, int>)NativeLibrary.GetExport(lib, "lp_blob_fetch");
            BlobHashText = (delegate* unmanaged[Cdecl]<nint, ulong, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lp_blob_hash_text");
            BookmarkPut = (delegate* unmanaged[Cdecl]<nint, byte*, uint, byte*, byte*, int>)NativeLibrary.GetExport(lib, "lp_bookmark_put");
            BookmarkForget = (delegate* unmanaged[Cdecl]<nint, byte*, int>)NativeLibrary.GetExport(lib, "lp_bookmark_forget");
            BookmarkList = (delegate* unmanaged[Cdecl]<nint, byte*, nuint, nuint*, int>)NativeLibrary.GetExport(lib, "lp_bookmark_list");

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
