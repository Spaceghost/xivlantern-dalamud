using XivLantern.Core;

namespace XivLantern.Core.Tests;

public class DtrRosterTests
{
    private static FriendView Friend(string name, bool online = true, string status = "online", string note = "")
        => new(new string('a', 64), name, online, status, note, null, false, 0);

    [Fact]
    public void RosterShowsBothCountsSortsOnlineFirstAndHidesOfflineNotes()
    {
        var roster = DtrRoster.Build([Friend("Aaron", false, note: "stale private note"),
            Friend("Cid", true, "busy", "prog"), Friend("Alice", true, "away", "crafting"), Friend("Bob")], 1, false);
        Assert.Equal("Lantern 3 · 1 unread", roster.Text);
        Assert.Equal(new[] { "Lantern: 3 friend(s) online, 1 unread. Click to open.",
            "Alice, away: crafting", "Bob, online", "Cid, busy: prog", "Aaron, offline" }, roster.Tooltip.Split('\n'));
        Assert.DoesNotContain("stale private note", roster.Tooltip);
    }

    [Fact]
    public void FriendTextCannotInjectControlPayloadsOrExtraRosterRows()
    {
        var roster = DtrRoster.Build([Friend("Alice\u0002\u0003\n##hidden", true, "bad\u0002status", "note\u0002\u0003\nsecond row")], 0, false);
        Assert.DoesNotContain('\u0002', roster.Tooltip);
        Assert.DoesNotContain('\u0003', roster.Tooltip);
        Assert.DoesNotContain("##", roster.Tooltip);
        Assert.Equal(2, roster.Tooltip.Split('\n').Length);
        Assert.Contains("online: note second row", roster.Tooltip);
        var bounded = DtrRoster.Build([Friend(new string('a', 1000), note: new string('b', 1000))], 0, false);
        Assert.True(bounded.Tooltip.Length < 200);
    }

    [Fact]
    public void LongRosterHasCapAndPrivacyModeHasNoNamesOrNotes()
    {
        var friends = Enumerable.Range(0, 15).Select(i => Friend($"Friend {i:D2}", note: "private note")).ToArray();
        var roster = DtrRoster.Build(friends, 3, false);
        Assert.Equal(DtrRoster.MaxFriends + 2, roster.Tooltip.Split('\n').Length);
        Assert.EndsWith("and 7 more", roster.Tooltip);
        Assert.DoesNotContain("Friend 08", roster.Tooltip);
        var hidden = DtrRoster.Build(friends, 3, true);
        Assert.Equal(roster.Text, hidden.Text);
        Assert.DoesNotContain("Friend", hidden.Tooltip);
        Assert.DoesNotContain("private note", hidden.Tooltip);
        Assert.Single(hidden.Tooltip.Split('\n'));
        Assert.Equal("Lantern 0", DtrRoster.Build([], -1, false).Text);
    }
}
