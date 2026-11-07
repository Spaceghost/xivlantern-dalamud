using XivLantern.Core;

namespace XivLantern.Core.Tests;

public class PullCountdownTests
{
    [Fact]
    public void ChannelResolutionNeverChoosesAnAmbiguousDestination()
    {
        var channels = new[] { ("c:raid", "Static"), ("c:fc", "FC") };
        Assert.Equal("c:raid", PullChannels.Resolve(channels, "#static", null));
        Assert.Equal("c:fc", PullChannels.Resolve(channels, "", "c:fc"));
        Assert.Null(PullChannels.Resolve(channels, "", "f:friend"));
        Assert.Null(PullChannels.Resolve(channels, "missing", "c:fc"));
        Assert.Null(PullChannels.Resolve([("c:1", "Static"), ("c:2", "static")], "Static", null));
        Assert.Equal("c:raid", PullChannels.Resolve([channels[0]], "", null));
        Assert.Null(PullChannels.Resolve([], "", null));
    }

    [Fact]
    public void CommandPreservesOptionalWholeChannelName()
    {
        Assert.Equal(new Command(Verb.Pull, "10", ""), Command.Parse("pull 10"));
        Assert.Equal(new Command(Verb.Pull, "15", "#Raid static"), Command.Parse("PULL 15 #Raid static"));
        Assert.Equal(new Command(Verb.Pull, "", ""), Command.Parse("pull"));
    }

    [Theory]
    [InlineData(1)]
    [InlineData(10)]
    [InlineData(60)]
    public void EnvelopeIsReadableAndDurationOnly(int seconds)
    {
        Assert.Equal(seconds, PullEnvelope.Parse(PullEnvelope.Encode(seconds)));
        Assert.Contains($"countdown: {seconds}s", PullEnvelope.Encode(seconds));
    }

    [Theory]
    [InlineData("ordinary chat")]
    [InlineData("[Lantern pull v2] countdown: 10s")]
    [InlineData("[Lantern pull v1] countdown: 0s")]
    [InlineData("[Lantern pull v1] countdown: 61s")]
    [InlineData("[Lantern pull v1] countdown: 010s")]
    [InlineData("[Lantern pull v1] countdown: +10s")]
    [InlineData("[Lantern pull v1] countdown: 10s extra")]
    [InlineData("[Lantern pull v1] countdown: 999999999999999999s")]
    public void MalformedOrUnversionedMessagesDoNotStartTimer(string message)
        => Assert.Null(PullEnvelope.Parse(message));

    [Fact]
    public void CountdownUsesMonotonicReceiptTimeAndCompletesOnlyOnce()
    {
        var timer = new PullCountdown();
        Assert.True(timer.Start(10, "c:static", "Static", 1000));
        Assert.Equal(10, timer.Remaining(1000));
        Assert.Equal(10, timer.Remaining(1001));
        Assert.Equal(9, timer.Remaining(2000));
        Assert.False(timer.Advance(10_999));
        Assert.True(timer.Advance(11_000));
        Assert.Equal(0, timer.Remaining(11_000));
        Assert.False(timer.Advance(11_001));
        Assert.Null(timer.Remaining(13_001));
        Assert.True(timer.Start(5, "c:next", "Next", 13_001));
        Assert.Equal("Next", timer.Label);
    }

    [Fact]
    public void FloodCannotRestartActiveTimerAndDisableClearsIt()
    {
        var timer = new PullCountdown();
        Assert.False(timer.Start(0, "c:raid", "Raid", 0));
        Assert.False(timer.Start(61, "c:raid", "Raid", 0));
        Assert.False(timer.Start(10, "", "", 0));
        Assert.True(timer.Start(10, "c:raid", "Raid", 0));
        Assert.False(timer.Start(60, "c:other", "Other", 9999));
        Assert.Equal("c:raid", timer.Channel);
        timer.Clear();
        Assert.Null(timer.Remaining(9999));
        Assert.False(timer.Advance(10_000));
    }

    [Fact]
    public void ResumeAfterLongPauseDoesNotRaiseLateCompletion()
    {
        var timer = new PullCountdown();
        timer.Start(5, "c:raid", "Raid", 0);
        Assert.False(timer.Advance(8000));
        Assert.False(timer.Advance(8001));
        Assert.Null(timer.Remaining(8000));
    }
}
