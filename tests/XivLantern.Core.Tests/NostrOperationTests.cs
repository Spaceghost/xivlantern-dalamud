namespace XivLantern.Core.Tests;

public class NostrOperationTests
{
    [Fact]
    public void QueuedNetworkJobStaysBusyUntilItsMatchingCompletion()
    {
        var operation = new NostrOperation();
        Assert.True(operation.Begin());
        operation.Prepared("queued", 42);
        Assert.True(operation.Busy);
        Assert.False(operation.Begin());
        operation.Completed(0, "wrong");
        operation.Completed(41, "wrong");
        Assert.True(operation.Busy);
        Assert.Equal("queued", operation.Status);
        operation.Completed(42, "failed");
        Assert.False(operation.Busy);
        Assert.Equal("failed", operation.Status);
        Assert.True(operation.Begin());
    }

    [Fact]
    public void CompletionBeforeWorkerHandleDoesNotLoseTheResultOrLeaveBusyStuck()
    {
        var operation = new NostrOperation();
        operation.Begin();
        operation.Completed(23, "unrelated");
        operation.Completed(24, "sent");
        Assert.True(operation.Busy);
        operation.Prepared("queued", 24);
        Assert.False(operation.Busy);
        Assert.Equal("sent", operation.Status);
        operation.Completed(23, "late unrelated");
        Assert.Equal("sent", operation.Status);
    }

    [Fact]
    public void LocalPreparationFailureAndResetReleaseControlsAndDropOldResults()
    {
        var operation = new NostrOperation();
        operation.Begin();
        operation.Completed(3, "early");
        operation.Prepared("invalid recipient");
        Assert.False(operation.Busy);
        Assert.Equal("invalid recipient", operation.Status);
        operation.Begin();
        operation.Prepared("queued", 4);
        operation.Reset();
        operation.Completed(4, "stale");
        Assert.False(operation.Busy);
        Assert.Empty(operation.Status);
        operation.Begin();
        operation.Prepared("queued", 5);
        Assert.True(operation.Busy);
    }
}
