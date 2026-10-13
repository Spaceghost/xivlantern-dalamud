using System.Text.Json;

namespace XivLantern.Core;

public enum FileOfferState { Offered, Fetching, Canceling, Canceled, Saved, Failed }

/// <summary>Remote metadata is a claim, not proof of the downloaded file's size or type.</summary>
public sealed record FileOffer(ulong Handle, ulong Room, string Peer, string Hash, ulong Size,
    string Name, FileOfferState State = FileOfferState.Offered, string Destination = "", ulong Done = 0, bool Withdrawn = false);

/// <summary>Session-only, bounded UI state. Receiving an offer never starts a transfer.
/// Native download admission must independently enforce the real stream size.</summary>
public sealed class FileOffers
{
    public const ulong MaxBytes = 16 * 1024 * 1024;
    public const int Capacity = 128;
    public const int PeerCapacity = 8;
    public const int ConcurrentFetches = 2;
    private readonly List<FileOffer> offers = [];
    public IReadOnlyList<FileOffer> All => offers;
    public FileOffer? Find(ulong handle) => offers.FirstOrDefault(o => o.Handle == handle);
    public void Withdraw(ulong handle)
    {
        var offer = Find(handle);
        if (offer?.State == FileOfferState.Fetching)
        {
            CancelRequested(handle);
            var index = offers.FindIndex(o => o.Handle == handle);
            offers[index] = offers[index] with { Withdrawn = true };
        }
        else if (offer?.State is FileOfferState.Canceling or FileOfferState.Saved)
        {
            var index = offers.FindIndex(o => o.Handle == handle);
            offers[index] = offers[index] with { Withdrawn = true };
        }
        else if (offer?.State != FileOfferState.Canceling && offer?.State != FileOfferState.Saved)
            offers.RemoveAll(o => o.Handle == handle);
    }

    private static bool IsHash(string text) => text.Length == 64
        && text.All(c => c is >= '0' and <= '9' or >= 'a' and <= 'f' or >= 'A' and <= 'F');

    public bool Add(ulong handle, string peer, string metadata, Func<ulong, bool> roomAllowed,
        Func<string, bool> peerAllowed)
    {
        if (handle == 0 || !IsHash(peer) || metadata.Length > 4096 || offers.Any(o => o.Handle == handle)) return false;
        peer = peer.ToLowerInvariant();
        if (!peerAllowed(peer) || offers.Count >= Capacity || offers.Count(o => o.Peer == peer) >= PeerCapacity) return false;
        try
        {
            using var json = JsonDocument.Parse(metadata, new JsonDocumentOptions { MaxDepth = 4 });
            var root = json.RootElement;
            if (root.ValueKind != JsonValueKind.Object) return false;
            var fields = root.EnumerateObject().ToArray();
            if (fields.Length != 4 || fields.Select(p => p.Name).Distinct(StringComparer.Ordinal).Count() != 4) return false;
            // Room zero is reserved for an authenticated friend offer. Caller
            // must admit its known friend identity, never treat it as a channel.
            if (!root.TryGetProperty("room", out var roomValue) || !roomValue.TryGetUInt64(out var room) || !roomAllowed(room)) return false;
            if (!root.TryGetProperty("size", out var sizeValue) || !sizeValue.TryGetUInt64(out var size) || size > MaxBytes) return false;
            if (!root.TryGetProperty("hash", out var hashValue) || hashValue.ValueKind != JsonValueKind.String) return false;
            if (!root.TryGetProperty("name", out var nameValue) || nameValue.ValueKind != JsonValueKind.String) return false;
            var hash = hashValue.GetString()!;
            var name = nameValue.GetString()!;
            if (!IsHash(hash) || name.Length is 0 or > 256 || name.Any(char.IsControl)) return false;
            // Display only: remove either platform's directory spelling. It never supplies a path.
            name = name.Replace('\\', '/').Split('/')[^1];
            if (string.IsNullOrWhiteSpace(name) || name is "." or "..") return false;
            hash = hash.ToLowerInvariant();
            if (offers.Any(o => o.Room == room && o.Peer == peer && o.Hash == hash)) return false;
            offers.Add(new(handle, room, peer, hash, size, name));
            return true;
        }
        catch (JsonException) { return false; }
        catch (InvalidOperationException) { return false; }
    }

    /// <summary>Called only after an explicit click and fresh room/block/mute checks.
    /// The caller creates the download directory privately and refuses symlink/reparse destinations.</summary>
    public string? BeginFetch(ulong handle, string directory, Func<FileOffer, bool> allowed)
    {
        var index = offers.FindIndex(o => o.Handle == handle);
        if (index < 0 || offers.Count(o => o.State is FileOfferState.Fetching or FileOfferState.Canceling) >= ConcurrentFetches) return null;
        var offer = offers[index];
        if (offer.State is not (FileOfferState.Offered or FileOfferState.Failed or FileOfferState.Canceled) || !allowed(offer)) return null;
        // Remote names, extensions, and completion payloads never choose a destination.
        var path = Path.Combine(Path.GetFullPath(directory), Guid.NewGuid().ToString("N") + ".download");
        offers[index] = offer with { State = FileOfferState.Fetching, Destination = path, Done = 0 };
        return path;
    }

    public bool Progress(ulong handle, ulong done)
    {
        var index = offers.FindIndex(o => o.Handle == handle && o.State == FileOfferState.Fetching);
        if (index < 0) return false;
        var offer = offers[index];
        if (done > offer.Size || done < offer.Done) return false;
        offers[index] = offer with { Done = done };
        return true;
    }

    /// <summary>The caller verifies actual size and content hash before reporting success.
    /// Native event text cannot redirect the saved path.</summary>
    public bool Complete(ulong handle, bool verified)
    {
        var index = offers.FindIndex(o => o.Handle == handle && o.State is FileOfferState.Fetching or FileOfferState.Canceling);
        if (index < 0) return false;
        if (!verified && offers[index].Withdrawn) { offers.RemoveAt(index); return true; }
        var canceled = offers[index].State == FileOfferState.Canceling;
        offers[index] = offers[index] with { State = verified ? FileOfferState.Saved : canceled ? FileOfferState.Canceled : FileOfferState.Failed };
        return true;
    }

    /// <summary>After the native cancellation request succeeds, keep the slot occupied
    /// until its terminal event. A successful completion may have raced the request.</summary>
    public bool CancelRequested(ulong handle)
    {
        var index = offers.FindIndex(o => o.Handle == handle && o.State == FileOfferState.Fetching);
        if (index < 0) return false;
        offers[index] = offers[index] with { State = FileOfferState.Canceling };
        return true;
    }

    public void RemoveWhere(Func<FileOffer, bool> remove) => offers.RemoveAll(o => remove(o));
    public void Clear() => offers.Clear();
}
