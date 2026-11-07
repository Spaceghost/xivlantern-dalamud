namespace XivLantern.Core;

public sealed class SharedCacheException(string message) : IOException(message);

/// <summary>Worker-only admission/explicit cleanup of Lantern's native serving
/// cache. The config directory must stay locally controlled. Observed links,
/// unknown contents and more than 4096 entries are refused before deletion.</summary>
public static class SharedFileCache
{
    public const ulong DefaultBudgetBytes = 256UL * 1024 * 1024;
    private const int MaxEntries = 4096;
    private sealed record Snapshot(string Root, List<string> Files, List<string> Directories, ulong Bytes);

    public static void CheckBudget(string configDirectory, ulong budgetBytes = DefaultBudgetBytes)
    {
        if (budgetBytes is < 64UL * 1024 * 1024 or > 1024UL * 1024 * 1024) throw new ArgumentOutOfRangeException(nameof(budgetBytes));
        Snapshot snapshot;
        try { snapshot = Inspect(configDirectory); }
        catch (SharedCacheException) { throw; }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        { throw new SharedCacheException("The shared-file cache could not be inspected. Turn Lantern off and check its cached files before sharing again."); }
        // Reserve two maximum file sizes for payload/outboard/database growth.
        // This is admission headroom, not a hard filesystem allocation quota.
        if (snapshot.Bytes > budgetBytes - 2 * FileOffers.MaxBytes)
            throw new SharedCacheException("The shared-file cache has reached its storage budget. Turn Lantern off and clear the shared-file cache before sharing again.");
    }

    /// <summary>Only call after the native node has fully stopped. Identity DB,
    /// originals and saved downloads are outside this fixed cache directory.</summary>
    public static ulong Clear(string configDirectory)
    {
        var snapshot = Inspect(configDirectory);
        if (!Directory.Exists(snapshot.Root)) return 0;
        foreach (var path in snapshot.Files)
        {
            CheckAncestors(Path.GetDirectoryName(path)!);
            if ((File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0) throw Refused();
            File.Delete(path);
        }
        // Non-recursive deletes preserve anything added after the snapshot.
        foreach (var path in snapshot.Directories) Directory.Delete(path, false);
        Directory.Delete(snapshot.Root, false);
        return snapshot.Bytes;
    }

    private static Snapshot Inspect(string configDirectory)
    {
        var root = Path.Combine(Path.GetFullPath(configDirectory), "lantern.blobs");
        CheckAncestors(root);
        var files = new List<string>(); var directories = new List<string>();
        ulong used = 0; var count = 0;
        if (!Directory.Exists(root)) return new(root, files, directories, 0);
        foreach (var path in Directory.EnumerateFileSystemEntries(root))
        {
            if (++count > MaxEntries) throw Refused();
            var attributes = File.GetAttributes(path);
            if ((attributes & FileAttributes.ReparsePoint) != 0) throw Refused();
            var name = Path.GetFileName(path);
            if ((attributes & FileAttributes.Directory) != 0)
            {
                if (name is not ("data" or "temp")) throw Refused();
                directories.Add(path);
                foreach (var entry in Directory.EnumerateFileSystemEntries(path))
                {
                    if (++count > MaxEntries) throw Refused();
                    var entryAttributes = File.GetAttributes(entry);
                    if ((entryAttributes & (FileAttributes.Directory | FileAttributes.ReparsePoint)) != 0 || !NativeName(name, Path.GetFileName(entry))) throw Refused();
                    Add(entry);
                }
            }
            else { if (name != "blobs.db") throw Refused(); Add(path); }
        }
        return new(root, files, directories, used);
        void Add(string path)
        {
            var length = (ulong)new FileInfo(path).Length;
            if (length > ulong.MaxValue - used) throw Refused();
            used += length; files.Add(path);
        }
    }
    private static bool NativeName(string directory, string name)
    {
        var stem = Path.GetFileNameWithoutExtension(name);
        var extension = Path.GetExtension(name);
        var length = directory == "temp" ? 32 : 64;
        return stem.Length == length && stem.All(c => c is >= '0' and <= '9' or >= 'a' and <= 'f')
            && (directory == "temp" ? extension == ".temp" : extension is ".data" or ".obao4" or ".sizes4" or ".bitfield");
    }
    private static SharedCacheException Refused() => new("The shared-file cache contains links, unknown entries or too many files.");
    private static void CheckAncestors(string path)
    {
        for (DirectoryInfo? directory = new(path); directory != null; directory = directory.Parent)
            if (directory.Exists && (directory.Attributes & FileAttributes.ReparsePoint) != 0) throw Refused();
    }
}
