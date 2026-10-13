namespace XivLantern.Core.Tests;

public class NostrInboxTests
{
    private const string Sender = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    private const string Ticket = "ltfriendaaaaaaaaaaa";

    [Fact]
    public void IgnoresSurviveReloadWithoutRetainingBearerTickets()
    {
        var inbox = new NostrInbox();
        Assert.True(inbox.Add(Sender, Ticket));
        Assert.False(inbox.Add(Sender.ToUpperInvariant(), Ticket.ToUpperInvariant()));
        var offer = Assert.Single(inbox.Pending);
        Assert.True(inbox.Ignore(offer.Key));
        Assert.Empty(inbox.Pending);
        Assert.DoesNotContain(Ticket, Assert.Single(inbox.IgnoredKeys));
        var reopened = new NostrInbox(inbox.IgnoredKeys);
        Assert.False(reopened.Add(Sender, Ticket));
        Assert.True(reopened.Add(new string('b', 64), Ticket));
    }

    [Fact]
    public void FailedAcceptanceRemainsAvailableAndSuccessRequiresMatchingHandle()
    {
        var inbox = new NostrInbox();
        inbox.Add(Sender, Ticket);
        var key = Assert.Single(inbox.Pending).Key;
        Assert.True(inbox.AcceptStarted(key, 7));
        Assert.False(inbox.Ignore(key));
        Assert.False(inbox.AcceptStarted(key, 8));
        Assert.False(inbox.Complete(0, true));
        Assert.False(inbox.Complete(8, true));
        Assert.True(inbox.Complete(7, false));
        Assert.Equal(0UL, Assert.Single(inbox.Pending).AcceptHandle);
        Assert.True(inbox.AcceptStarted(key, 9));
        Assert.True(inbox.Complete(9, true));
        Assert.Empty(inbox.Pending);
        Assert.False(inbox.Add(Sender, Ticket));
    }

    [Fact]
    public void InboxAndStoredIgnoresStayBoundedAndRejectMalformedInput()
    {
        var inbox = new NostrInbox();
        Assert.False(inbox.Add("bad", Ticket));
        Assert.False(inbox.Add(Sender, "prefix " + Ticket));
        Assert.False(inbox.Add(Sender, "ltroomaaaaaaaaaaa"));
        Assert.False(inbox.Add(Sender, "ltfriend" + new string('a', 4096)));
        long clock = 0;
        inbox = new NostrInbox(monotonicMilliseconds: () => clock);
        for (int i = 0; i < 400; i++)
        {
            clock += NostrInbox.WindowMilliseconds;
            Assert.True(inbox.Add(i.ToString("x64"), Ticket));
            Assert.True(inbox.Ignore(Assert.Single(inbox.Pending).Key));
        }
        Assert.Equal(NostrInbox.IgnoreCapacity, inbox.IgnoredKeys.Count);
        for (int i = 0; i < NostrInbox.Capacity; i++) Assert.True(inbox.Add((1000 + i).ToString("x64"), Ticket));
        Assert.False(inbox.Add(new string('f', 64), Ticket));
        Assert.Equal(NostrInbox.Capacity, inbox.Pending.Count);
        inbox.ClearPending();
        Assert.Empty(inbox.Pending);
    }
    [Fact]
    public void OneSenderCannotFillTheInboxAndIgnoreReleasesItsSlot()
    {
        var inbox = new NostrInbox();
        for (int i = 0; i < NostrInbox.SenderCapacity; i++)
            Assert.True(inbox.Add(Sender, Ticket + new string('a', i)));
        Assert.False(inbox.Add(Sender, Ticket + "bbbb"));
        Assert.True(inbox.Add(new string('b', 64), Ticket));
        var first = inbox.Pending.First(p => p.Sender == Sender);
        Assert.True(inbox.Ignore(first.Key));
        Assert.True(inbox.Add(Sender, Ticket + "bbbb"));
        Assert.False(inbox.Add(Sender, first.Ticket));
    }

    [Fact]
    public void IgnoringOrClearingPendingDoesNotBypassSenderTimeWindow()
    {
        long now = 1000;
        var inbox = new NostrInbox(monotonicMilliseconds: () => now);
        for (int i = 0; i < NostrInbox.SenderWindowLimit; i++)
        {
            Assert.True(inbox.Add(Sender, Ticket + new string('a', i)));
            Assert.True(inbox.Ignore(Assert.Single(inbox.Pending).Key));
        }
        var next = Ticket + new string('b', 20);
        Assert.False(inbox.Add(Sender, next));
        inbox.ClearPending();
        now = 0; // a clock rollback must not create a fresh window
        Assert.False(inbox.Add(Sender, next));
        now = 1000 + NostrInbox.WindowMilliseconds;
        Assert.True(inbox.Add(Sender, next));
    }

    [Fact]
    public void RotatingSenderKeysCannotBypassTheGlobalTimeWindow()
    {
        long now = 0;
        var inbox = new NostrInbox(monotonicMilliseconds: () => now);
        for (int i = 0; i < NostrInbox.WindowLimit; i++)
        {
            Assert.True(inbox.Add(i.ToString("x64"), Ticket));
            inbox.ClearPending();
        }
        Assert.False(inbox.Add(new string('f', 64), Ticket));
        now += NostrInbox.WindowMilliseconds;
        Assert.True(inbox.Add(new string('f', 64), Ticket));
    }

}
