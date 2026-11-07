namespace XivLantern.Core;

public sealed class DownloadBudgetException() : IOException("The download folder has reached its storage budget. Remove saved downloads before trying again.");

public static class FileDownloads
{
    public const ulong DefaultBudgetBytes = 256 * 1024 * 1024;
    /// <summary>Worker-only snapshot of an explicitly chosen local file. It is not
    /// published here. The byte cap applies while reading, even if the file grows.</summary>
    public static (string Name, byte[] Bytes) ReadOffer(string path)
    {
        if (string.IsNullOrWhiteSpace(path) || path.Contains('\0')) throw new ArgumentException("Choose a file first.", nameof(path));
        var name = Path.GetFileName(path);
        if (name.Length is 0 or > 256 || name.Any(char.IsControl)) throw new ArgumentException("Choose a file with a valid name.", nameof(path));
        using var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        if ((ulong)file.Length > FileOffers.MaxBytes) throw new IOException("Shared files must be at most 16 MiB.");
        using var bytes = new MemoryStream();
        var buffer = new byte[4096];
        while (true)
        {
            var count = file.Read(buffer);
            if (count == 0) break;
            if ((ulong)bytes.Length + (ulong)count > FileOffers.MaxBytes) throw new IOException("Shared files must be at most 16 MiB.");
            bytes.Write(buffer, 0, count);
        }
        return (name, bytes.ToArray());
    }

    /// <summary>Worker-only filesystem preparation. Refuse observed linked ancestors;
    /// the directory must remain locally controlled throughout a transfer.</summary>
    public static string PrepareDirectory(string configDirectory, ulong budgetBytes = DefaultBudgetBytes)
    {
        var root = Path.GetFullPath(configDirectory);
        CheckAncestors(root);
        var downloads = Path.Combine(root, "downloads");
        CheckAncestors(downloads);
        Directory.CreateDirectory(downloads);
        CheckAncestors(downloads);
        CheckBudget(downloads, budgetBytes);
        return downloads;
    }

    private static void CheckBudget(string directory, ulong budgetBytes)
    {
        var reservation = FileOffers.MaxBytes * FileOffers.ConcurrentFetches;
        if (budgetBytes < reservation || budgetBytes > 1024UL * 1024 * 1024)
            throw new ArgumentOutOfRangeException(nameof(budgetBytes));
        ulong used = 0;
        var count = 0;
        foreach (var path in Directory.EnumerateFiles(directory))
        {
            if (++count > 4096) throw new DownloadBudgetException();
            var file = new FileInfo(path);
            if ((file.Attributes & FileAttributes.ReparsePoint) != 0)
                throw new IOException("The download folder cannot contain linked files.");
            var size = (ulong)file.Length;
            if (size > budgetBytes - reservation - used) throw new DownloadBudgetException();
            used += size;
        }
        // Reserve room for both native jobs, even when neither destination exists
        // yet. Concurrent preparations may share this same conservative reserve.
    }

    /// <summary>Opt-in worker cleanup of Lantern's generated download names only.
    /// Active/preparing destinations are excluded; unrelated files are preserved.</summary>
    public static IReadOnlyList<string> CleanupExpired(string configDirectory, int days,
        IEnumerable<string> protectedPaths, DateTime utcNow)
    {
        if (days is < 1 or > 90) throw new ArgumentOutOfRangeException(nameof(days));
        var directory = Path.Combine(Path.GetFullPath(configDirectory), "downloads");
        CheckAncestors(directory);
        if (!Directory.Exists(directory)) return [];
        var protectedSet = protectedPaths.Select(Path.GetFullPath).ToHashSet(OperatingSystem.IsWindows()
            ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal);
        var cutoff = utcNow.ToUniversalTime().AddDays(-days);
        var deleted = new List<string>();
        var paths = Directory.EnumerateFiles(directory).Take(4097).ToArray();
        if (paths.Length > 4096) throw new DownloadBudgetException();
        foreach (var path in paths)
        {
            if (protectedSet.Contains(path) || Path.GetExtension(path) != ".download"
                || !Guid.TryParseExact(Path.GetFileNameWithoutExtension(path), "N", out _)) continue;
            try
            {
                var file = new FileInfo(path);
                if ((file.Attributes & FileAttributes.ReparsePoint) != 0 || file.LastWriteTimeUtc >= cutoff) continue;
                file.Delete();
                deleted.Add(path);
            }
            catch (IOException) { /* Locked or disappearing files stay outside successful cleanup results. */ }
            catch (UnauthorizedAccessException) { }
        }
        return deleted;
    }

    private static void CheckAncestors(string path)
    {
        for (DirectoryInfo? directory = new(path); directory != null; directory = directory.Parent)
            if (directory.Exists && (directory.Attributes & FileAttributes.ReparsePoint) != 0)
                throw new IOException("The download directory cannot use symbolic links or reparse points.");
    }
}
