namespace XivLantern.Core.Tests;

public class ChannelHistoryTests
{
    private static ChatLine Signed(char signature, long time, bool outgoing = false) =>
        new(DateTimeOffset.FromUnixTimeMilliseconds(time), "author", "repeated text", outgoing,
            Verified: true, HistorySignature: new string(signature, 128));

    [Fact]
    public void LiveThenBacklogMergesSignaturesAndKeepsRepeatedText()
    {
        var c = new Conversation("c:test", "test");
        c.Add(Signed('b', 20), true);
        c.LoadHistory([Signed('a', 10), Signed('b', 20), Signed('a', 10)]);
        Assert.Equal(2, c.Lines.Count);
        Assert.Equal(new string('a', 128), c.Lines[0].HistorySignature);
        Assert.Equal(new string('b', 128), c.Lines[1].HistorySignature);
        Assert.Equal(1, c.Unread); // Catch-up does not create unread notifications.
        c.Add(Signed('a', 10), true);
        Assert.Equal(2, c.Lines.Count);
        Assert.Equal(1, c.Unread);
    }

    [Fact]
    public void BacklogThenLiveDoesNotReplayOrDeduplicateUnrelatedLegacyText()
    {
        var c = new Conversation("c:test", "test");
        c.LoadHistory([Signed('a', 10)]);
        c.Add(Signed('a', 10), true);
        Assert.Equal(0, c.Unread);
        c.Add(new ChatLine(DateTimeOffset.Now, "legacy", "repeated text", false), true);
        c.Add(Signed('b', 20), true);
        Assert.Equal(3, c.Lines.Count);
        Assert.Equal(2, c.Unread);
    }

    [Fact]
    public void OwnLiveRecordAndReturnedSnapshotShareOneIdentity()
    {
        var c = new Conversation("c:test", "test");
        c.Add(Signed('a', 10, true), false);
        c.LoadHistory([Signed('a', 10, true)]);
        Assert.Single(c.Lines);
        Assert.True(c.Lines[0].Outgoing);
        Assert.Equal(0, c.Unread);
    }

    [Fact]
    public void FullSignaturesTakePrecedenceOverTheFriendMessageIdField()
    {
        var c = new Conversation("c:test", "test");
        c.Add(Signed('a', 10) with { Id = 1 }, true);
        c.Add(Signed('b', 20) with { Id = 1 }, true);
        c.LoadHistory([Signed('a', 10) with { Id = 1 }]);
        Assert.Equal(2, c.Lines.Count);
        Assert.Equal(2, c.Unread);
    }

    [Fact]
    public void UnrepresentableSenderDatesStillSortByTheOriginalClaim()
    {
        var c = new Conversation("c:test", "test");
        c.LoadHistory([Signed('b', 0) with { ClaimedAt = long.MaxValue }, Signed('a', 1000) with { ClaimedAt = 10 }]);
        Assert.Equal(10, c.Lines[0].ClaimedAt);
        Assert.Equal(long.MaxValue, c.Lines[1].ClaimedAt);
    }

    [Fact]
    public void MergedChannelHistoryStillHonorsConversationLimit()
    {
        var c = new Conversation("c:test", "test");
        c.LoadHistory(Enumerable.Range(0, 700).Select(i => new ChatLine(
            DateTimeOffset.FromUnixTimeMilliseconds(i), "author", "text", false,
            Verified: true, HistorySignature: i.ToString("x128"))));
        Assert.Equal(Conversation.MaxLines, c.Lines.Count);
        Assert.Equal(DateTimeOffset.FromUnixTimeMilliseconds(200), c.Lines[0].Time);
        Assert.Equal(0, c.Unread);
    }
}
