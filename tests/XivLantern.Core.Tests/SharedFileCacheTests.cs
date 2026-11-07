namespace XivLantern.Core.Tests;

public sealed class SharedFileCacheTests : IDisposable
{
    private readonly string root = Directory.CreateTempSubdirectory("lantern-cache-tests-").FullName;
    public void Dispose() => Directory.Delete(root, true);
    private string Cache => Path.Combine(root, "lantern.blobs");
    private string Populate()
    {
        Directory.CreateDirectory(Path.Combine(Cache, "data"));
        Directory.CreateDirectory(Path.Combine(Cache, "temp"));
        File.WriteAllText(Path.Combine(Cache,"blobs.db"), "cached metadata");
        var file = Path.Combine(Cache,"data",new string('a',64)+".data");
        File.WriteAllText(file,"cached bytes");
        File.WriteAllText(Path.Combine(Cache,"temp",new string('b',32)+".temp"),"cached temp");
        return file;
    }
    [Fact]
    public void ClearRemovesOnlyKnownCacheAndKeepsIdentityOriginalsAndDownloads()
    {
        Populate();
        var keep = new[] { Path.Combine(root,"lantern.sqlite"), Path.Combine(root,"clear.png"), Path.Combine(root,"downloads","saved.download") };
        Directory.CreateDirectory(Path.Combine(root,"downloads"));
        foreach (var path in keep) File.WriteAllText(path,"keep");
        SharedFileCache.CheckBudget(root);
        Assert.True(SharedFileCache.Clear(root) > 0);
        Assert.False(Directory.Exists(Cache));
        foreach (var path in keep) Assert.Equal("keep", File.ReadAllText(path));
        Assert.Equal(0UL, SharedFileCache.Clear(root));
    }
    [Fact]
    public void CacheBudgetCountsPersistedDataAndReservesAdmissionHeadroom()
    {
        var path = Populate();
        using (var file = File.OpenWrite(path)) file.SetLength(32 * 1024 * 1024);
        Assert.Throws<SharedCacheException>(() => SharedFileCache.CheckBudget(root,64UL * 1024 * 1024));
        Assert.True(File.Exists(path));
        SharedFileCache.CheckBudget(root,65UL * 1024 * 1024);
        Assert.Throws<ArgumentOutOfRangeException>(() => SharedFileCache.CheckBudget(root,1));
    }
    [Fact]
    public void UnknownContentsAndExcessEntriesAreRefusedBeforeAnyDeletion()
    {
        var path = Populate();
        var unknown = Path.Combine(Cache,"personal.txt"); File.WriteAllText(unknown,"personal");
        Assert.Throws<SharedCacheException>(() => SharedFileCache.Clear(root));
        Assert.True(File.Exists(path)); Assert.Equal("personal",File.ReadAllText(unknown));
        File.Delete(unknown);
        for (var i = 0; i < 4096; i++) File.WriteAllText(Path.Combine(Cache,"temp",i.ToString("x32")+".temp"),"");
        Assert.Throws<SharedCacheException>(() => SharedFileCache.Clear(root));
        Assert.True(File.Exists(path));
    }
    [Fact]
    public void ObservedDirectoryAndFileLinksAreRefusedWithoutTouchingTheirTargets()
    {
        if (OperatingSystem.IsWindows()) return; // Actual Windows link behavior is a separate runtime gate.
        var path = Populate(); var target = Path.Combine(root,"original.png"); File.WriteAllText(target,"original");
        var link = Path.Combine(Cache,"data",new string('c',64)+".data"); File.CreateSymbolicLink(link,target);
        Assert.Throws<SharedCacheException>(() => SharedFileCache.Clear(root)); Assert.True(File.Exists(path)); Assert.Equal("original",File.ReadAllText(target));
        File.Delete(link); Directory.CreateSymbolicLink(Path.Combine(Cache,"other"),root);
        Assert.Throws<SharedCacheException>(() => SharedFileCache.CheckBudget(root)); Assert.Throws<SharedCacheException>(() => SharedFileCache.Clear(root)); Assert.True(File.Exists(path));
    }
}
