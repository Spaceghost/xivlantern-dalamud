using XivLantern.Core;

namespace XivLantern.Core.Tests;

public class IdleAwayTests
{
    [Fact]
    public void AwayAndRestorePublishOnceAndPreserveNote()
    {
        var policy = new IdleAway();
        var sent = new List<(uint Presence, string Note)>();
        bool Publish(uint presence, string note) { sent.Add((presence, note)); return true; }
        policy.Tick(true, true, 1, "maps?", 999, 1000, Publish);
        Assert.Empty(sent);
        policy.Tick(true, true, 1, "maps?", 1000, 1000, Publish);
        Assert.True(policy.Active);
        policy.Tick(true, true, 1, "maps?", 2000, 1000, Publish);
        Assert.Single(sent);
        policy.Tick(true, true, 1, "maps?", 0, 1000, Publish);
        policy.Tick(true, true, 1, "maps?", 1, 1000, Publish);
        Assert.False(policy.Active);
        Assert.Equal(new[] { (2u, "maps?"), (1u, "maps?") }, sent);
    }

    [Theory]
    [InlineData(0u)]
    [InlineData(2u)]
    [InlineData(3u)]
    public void ExplicitPresenceNeverBecomesAutomaticallyAway(uint presence)
    {
        var policy = new IdleAway();
        policy.Tick(true, true, presence, "manual", 9999, 1000, (_, _) => throw new Exception("must not publish"));
        Assert.False(policy.Active);
    }

    [Fact]
    public void ManualSelectionCancelsRestorationAndWaitsForNewActivity()
    {
        var policy = new IdleAway();
        policy.Tick(true, true, 1, "old", 1000, 1000, (_, _) => true);
        policy.ManualChanged();
        Assert.False(policy.Active);
        policy.Tick(true, true, 1, "new", 1001, 1000, (_, _) => throw new Exception("manual wins"));
        policy.Tick(true, true, 1, "new", 0, 1000, (_, _) => throw new Exception("still active"));
        string? published = null;
        policy.Tick(true, true, 1, "new", 1000, 1000, (_, note) => { published = note; return true; });
        Assert.Equal("new", published);
        Assert.True(policy.Active);
    }

    [Fact]
    public void FailedPublicationsDoNotCommitTransitions()
    {
        var policy = new IdleAway();
        policy.Tick(true, true, 1, "note", 1000, 1000, (_, _) => false);
        Assert.False(policy.Active);
        policy.Tick(true, true, 1, "note", 1001, 1000, (_, _) => true);
        policy.Tick(true, true, 1, "note", 0, 1000, (_, _) => false);
        Assert.True(policy.Active);
        policy.Tick(true, true, 1, "note", 1, 1000, (_, _) => true);
        Assert.False(policy.Active);
    }

    [Theory]
    [InlineData(false, 1000L)]
    [InlineData(true, null)]
    [InlineData(true, -1L)]
    public void DisablingOrUnavailableSampleRestoresManualPresence(bool enabled, long? idle)
    {
        var policy = new IdleAway();
        policy.Tick(true, true, 1, "note", 1000, 1000, (_, _) => true);
        uint? restored = null;
        policy.Tick(enabled, true, 1, "note", idle, 1000, (presence, _) => { restored = presence; return true; });
        Assert.Equal(1u, restored);
        Assert.False(policy.Active);
        policy.Tick(enabled, true, 1, "note", idle, 1000, (_, _) => throw new Exception("no false away"));
    }

    [Fact]
    public void DisconnectResetsWithoutCallingClosedNodeAndConfigChangeWins()
    {
        var policy = new IdleAway();
        policy.Tick(true, true, 1, "note", 1000, 1000, (_, _) => true);
        policy.Tick(true, false, 1, "note", 1000, 1000, (_, _) => throw new Exception("closed node"));
        Assert.False(policy.Active);
        policy.Tick(true, true, 1, "note", 1000, 1000, (_, _) => true);
        uint? restored = null;
        policy.Tick(true, true, 3, "new note", 1001, 1000, (presence, _) => { restored = presence; return true; });
        Assert.Equal(3u, restored);
        Assert.False(policy.Active);
    }

    [Fact]
    public void TickWrapIsHandledAndFutureTimestampIsUnavailable()
    {
        Assert.Equal(100L, IdleAway.Elapsed(200, 100));
        Assert.Equal(21L, IdleAway.Elapsed(10, uint.MaxValue - 10));
        Assert.Null(IdleAway.Elapsed(100, 101));
    }
}
