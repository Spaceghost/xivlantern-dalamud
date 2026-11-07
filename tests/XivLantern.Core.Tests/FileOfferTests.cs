using System.Text.Json;

namespace XivLantern.Core.Tests;

public class FileOfferTests
{
    [Fact]
    public void FriendScopeRequiresExplicitOriginAdmissionAndNeverStartsFetching()
    {
        var model = new FileOffers();
        Assert.False(model.Add(1, Peer, Metadata(room: 0), r => r > 0, _ => true));
        Assert.False(model.Add(1, Peer, Metadata(room: 0), r => r == 0, _ => false));
        Assert.True(model.Add(1, Peer, Metadata(room: 0), r => r == 0, _ => true));
        var offer = Assert.Single(model.All);
        Assert.Equal(0UL, offer.Room);
        Assert.Equal(FileOfferState.Offered, offer.State);
        Assert.Empty(offer.Destination);
    }

    [Fact]
    public void WithdrawalHidesUnusedOffersAndWaitsForActiveCleanupWithoutDeletingSavedFiles()
    {
        var model = new FileOffers();
        for (ulong i = 1; i <= 3; i++) Assert.True(model.Add(i, Peer, Metadata(hash: i.ToString("x64")), _ => true, _ => true));
        model.Withdraw(1);
        Assert.Null(model.Find(1));
        Assert.NotNull(model.BeginFetch(2, Path.GetTempPath(), _ => true));
        model.Withdraw(2);
        Assert.Equal(FileOfferState.Canceling, model.Find(2)!.State);
        Assert.True(model.Complete(2, false));
        Assert.Null(model.Find(2));
        var path = model.BeginFetch(3, Path.GetTempPath(), _ => true);
        Assert.True(model.Complete(3, true));
        model.Withdraw(3);
        Assert.Equal(FileOfferState.Saved, model.Find(3)!.State);
        Assert.Equal(path, model.Find(3)!.Destination);
        Assert.True(model.Find(3)!.Withdrawn);
        model.Withdraw(3);
        Assert.Equal(FileOfferState.Saved, model.Find(3)!.State);
        Assert.Null(model.BeginFetch(3, Path.GetTempPath(), _ => true));
    }

    private static readonly string Peer = new('a', 64);
    private static string Metadata(ulong size = 10, string name = "clear.png", ulong room = 7, string? hash = null)
        => JsonSerializer.Serialize(new { room, hash = hash ?? new string('b', 64), size, name });

    [Fact]
    public void CancellationKeepsItsSlotUntilTerminalResultAndAllowsCompletionRace()
    {
        var model = new FileOffers();
        for (ulong i = 1; i <= 3; i++) Assert.True(model.Add(i, Peer, Metadata(hash: i.ToString("x64")), _ => true, _ => true));
        Assert.NotNull(model.BeginFetch(1, Path.GetTempPath(), _ => true));
        Assert.NotNull(model.BeginFetch(2, Path.GetTempPath(), _ => true));
        Assert.True(model.CancelRequested(1));
        Assert.False(model.CancelRequested(1));
        Assert.Null(model.BeginFetch(3, Path.GetTempPath(), _ => true));
        Assert.False(model.Progress(1, 5));
        Assert.True(model.Complete(1, false));
        Assert.Equal(FileOfferState.Canceled, model.All.First(o => o.Handle == 1).State);
        Assert.NotNull(model.BeginFetch(3, Path.GetTempPath(), _ => true));
        Assert.True(model.CancelRequested(2));
        Assert.True(model.Complete(2, true));
        Assert.Equal(FileOfferState.Saved, model.All.First(o => o.Handle == 2).State);
    }

    [Fact]
    public void ReceivingAnOfferDoesNotFetchAndRemoteNamesNeverChoosePaths()
    {
        var model = new FileOffers();
        Assert.True(model.Add(1, Peer, Metadata(name: "../../folder\\clear.png"), r => r == 7, _ => true));
        var offer = Assert.Single(model.All);
        Assert.Equal("clear.png", offer.Name);
        Assert.Equal(FileOfferState.Offered, offer.State);
        Assert.Empty(offer.Destination);
        var directory = Path.Combine(Path.GetTempPath(), "lantern-offer-tests");
        var path = model.BeginFetch(1, directory, _ => true)!;
        Assert.Equal(Path.GetFullPath(directory), Path.GetDirectoryName(path));
        Assert.EndsWith(".download", path);
        Assert.DoesNotContain("clear", path);
        Assert.Null(model.BeginFetch(1, directory, _ => true));
        Assert.True(model.Complete(1, true));
        Assert.Equal(path, Assert.Single(model.All).Destination);
    }

    [Fact]
    public void InvalidMetadataAndDisallowedOriginsAreRejected()
    {
        var model = new FileOffers();
        Assert.False(model.Add(1, Peer, Metadata(FileOffers.MaxBytes + 1), _ => true, _ => true));
        Assert.False(model.Add(1, Peer, Metadata(), _ => false, _ => true));
        Assert.False(model.Add(1, Peer, Metadata(), _ => true, _ => false));
        Assert.False(model.Add(1, Peer, Metadata(name: "bad\nname"), _ => true, _ => true));
        Assert.False(model.Add(1, Peer, Metadata(hash: "bad"), _ => true, _ => true));
        Assert.False(model.Add(1, Peer, Metadata().Replace("\"room\":7", "\"room\":7,\"room\":8"), _ => true, _ => true));
        Assert.False(model.Add(1, Peer, "[]", _ => true, _ => true));
        Assert.False(model.Add(1, Peer, "{\"room\":\"seven\"}", _ => true, _ => true));
        Assert.Empty(model.All);
    }

    [Fact]
    public void DuplicateAndPerPeerFloodsStayBounded()
    {
        var model = new FileOffers();
        Assert.True(model.Add(1, Peer, Metadata(), _ => true, _ => true));
        Assert.False(model.Add(2, Peer.ToUpperInvariant(), Metadata(), _ => true, _ => true));
        for (ulong i = 2; i <= FileOffers.PeerCapacity; i++)
            Assert.True(model.Add(i, Peer, Metadata(hash: i.ToString("x64")), _ => true, _ => true));
        Assert.False(model.Add(99, Peer, Metadata(hash: new string('c', 64)), _ => true, _ => true));
        Assert.Equal(FileOffers.PeerCapacity, model.All.Count);
        model.RemoveWhere(o => o.Peer == Peer);
        Assert.Empty(model.All);
    }

    [Fact]
    public void FetchClicksRecheckAdmissionAndCompletionCannotReviveStaleOffers()
    {
        var model = new FileOffers();
        for (ulong i = 1; i <= 3; i++) Assert.True(model.Add(i, Peer, Metadata(hash: i.ToString("x64")), _ => true, _ => true));
        Assert.Null(model.BeginFetch(1, Path.GetTempPath(), _ => false));
        Assert.NotNull(model.BeginFetch(1, Path.GetTempPath(), _ => true));
        Assert.NotNull(model.BeginFetch(2, Path.GetTempPath(), _ => true));
        Assert.Null(model.BeginFetch(3, Path.GetTempPath(), _ => true));
        Assert.True(model.Progress(1, 5));
        Assert.False(model.Progress(1, 4));
        Assert.False(model.Progress(1, 11));
        Assert.False(model.Complete(3, true));
        Assert.True(model.Complete(1, false));
        Assert.NotNull(model.BeginFetch(3, Path.GetTempPath(), _ => true));
        model.Clear();
        Assert.False(model.Complete(2, true));
    }
}
