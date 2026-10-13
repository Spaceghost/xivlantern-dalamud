namespace XivLantern.Core.Tests;

public class FileDownloadTests
{
    [Fact]
    public void RetentionDeletesOnlyOldGeneratedFilesAndPreservesActiveAndUnrelatedFiles()
    {
        var root = Path.Combine(Path.GetTempPath(), "lantern-retention-" + Guid.NewGuid().ToString("N"));
        try
        {
            var folder = FileDownloads.PrepareDirectory(root);
            var old = Path.Combine(folder, Guid.NewGuid().ToString("N") + ".download");
            var active = Path.Combine(folder, Guid.NewGuid().ToString("N") + ".download");
            var other = Path.Combine(folder, "personal.download");
            var now = DateTime.UtcNow;
            foreach (var path in new[] { old, active, other })
            {
                File.WriteAllText(path, "kept unless eligible");
                File.SetLastWriteTimeUtc(path, now.AddDays(-10));
            }
            Assert.Equal(old, Assert.Single(FileDownloads.CleanupExpired(root, 7, [active], now)));
            Assert.False(File.Exists(old));
            Assert.True(File.Exists(active));
            Assert.True(File.Exists(other));
            Assert.Throws<ArgumentOutOfRangeException>(() => FileDownloads.CleanupExpired(root, 0, [], now));
        }
        finally { if (Directory.Exists(root)) Directory.Delete(root, true); }
    }

    [Fact]
    public void BudgetReservesBothConcurrentTransfersAndNeverDeletesExistingFiles()
    {
        var root = Path.Combine(Path.GetTempPath(), "lantern-budget-" + Guid.NewGuid().ToString("N"));
        try
        {
            var folder = FileDownloads.PrepareDirectory(root);
            var path = Path.Combine(folder, "saved.download");
            using (var file = File.Create(path)) file.SetLength(8 * 1024 * 1024);
            const ulong budget = 40 * 1024 * 1024;
            Assert.Equal(folder, FileDownloads.PrepareDirectory(root, budget));
            using (var file = File.OpenWrite(path)) file.SetLength(8 * 1024 * 1024 + 1);
            Assert.Throws<DownloadBudgetException>(() => FileDownloads.PrepareDirectory(root, budget));
            Assert.Equal(8 * 1024 * 1024 + 1, new FileInfo(path).Length);
            Assert.Throws<ArgumentOutOfRangeException>(() => FileDownloads.PrepareDirectory(root, 1));
        }
        finally { if (Directory.Exists(root)) Directory.Delete(root, true); }
    }

    [Fact]
    public void OfferSnapshotIsExactAndRejectsOversizedFilesWithoutPublishing()
    {
        var root = Path.Combine(Path.GetTempPath(), "lantern-offer-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);
        try
        {
            var path = Path.Combine(root, "clear.png");
            var expected = new byte[] { 1, 2, 3, 255 };
            File.WriteAllBytes(path, expected);
            var snapshot = FileDownloads.ReadOffer(path);
            Assert.Equal("clear.png", snapshot.Name);
            Assert.Equal(expected, snapshot.Bytes);
            using (var file = new FileStream(path, FileMode.Open, FileAccess.Write)) file.SetLength((long)FileOffers.MaxBytes + 1);
            Assert.Throws<IOException>(() => FileDownloads.ReadOffer(path));
            Assert.Throws<ArgumentException>(() => FileDownloads.ReadOffer("bad\0path"));
        }
        finally { Directory.Delete(root, true); }
    }

    [Fact]
    public void PreparationCreatesOnlyTheDownloadFolder()
    {
        var root = Path.Combine(Path.GetTempPath(), "lantern-download-" + Guid.NewGuid().ToString("N"));
        try
        {
            var result = FileDownloads.PrepareDirectory(root);
            Assert.Equal(Path.Combine(root, "downloads"), result);
            Assert.True(Directory.Exists(result));
            Assert.Empty(Directory.EnumerateFileSystemEntries(result));
            Assert.Equal(result, FileDownloads.PrepareDirectory(root));
        }
        finally { if (Directory.Exists(root)) Directory.Delete(root, true); }
    }

    [Fact]
    public void LinkedDownloadFolderCannotRedirectWrites()
    {
        if (OperatingSystem.IsWindows()) return; // Windows reparse admission needs its own runtime gate.
        var root = Path.Combine(Path.GetTempPath(), "lantern-download-" + Guid.NewGuid().ToString("N"));
        var target = root + "-target";
        try
        {
            Directory.CreateDirectory(root);
            Directory.CreateDirectory(target);
            Directory.CreateSymbolicLink(Path.Combine(root, "downloads"), target);
            Assert.Throws<IOException>(() => FileDownloads.PrepareDirectory(root));
            Assert.Empty(Directory.EnumerateFileSystemEntries(target));
        }
        finally
        {
            if (Directory.Exists(root)) Directory.Delete(root, true);
            if (Directory.Exists(target)) Directory.Delete(target, true);
        }
    }
}
