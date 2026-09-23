using System.Reflection;
using System.Text.Json;
using XivLinkpearl.Core;
using XivLinkpearl.Shared;

namespace XivLinkpearl.Core.Tests;

public class CommandTests
{
    [Theory]
    [InlineData("", Verb.Toggle)]
    [InlineData("  help ", Verb.Help)]
    [InlineData("settings", Verb.Settings)]
    [InlineData("ON", Verb.On)]
    [InlineData("off", Verb.Off)]
    [InlineData("selftest", Verb.SelfTest)]
    [InlineData("invite", Verb.Invite)]
    [InlineData("frobnicate", Verb.Unknown)]
    public void Verbs(string args, Verb verb) => Assert.Equal(verb, Command.Parse(args).Verb);

    [Fact]
    public void MessagesKeepTheirSpacing()
    {
        var c = Command.Parse("msg Alice  shall we  queue?");
        Assert.Equal((Verb.Msg, "Alice", "shall we  queue?"), (c.Verb, c.Arg, c.Text));
        var s = Command.Parse("status Away  brb, crafting");
        Assert.Equal((Verb.Status, "away", "brb, crafting"), (s.Verb, s.Arg, s.Text));
        Assert.Equal("lpfriendabc", Command.Parse("accept lpfriendabc").Arg);
    }

    [Theory]
    [InlineData("online", 1u)]
    [InlineData("afk", 2u)]
    [InlineData("dnd", 3u)]
    [InlineData("invisible", 0u)]
    [InlineData("sleepy", null)]
    public void PresenceWords(string word, uint? presence) => Assert.Equal(presence, Command.PresenceOf(word));
}

public class TextAndTicketTests
{
    [Fact]
    public void ClampNeverSplitsACharacter()
    {
        var s = new string('a', 2046) + "☕☕";   // ☕ is 3 bytes
        var c = ChatText.Clamp(s, ChatText.MaxChatBytes);
        Assert.Equal(2046, ChatText.Utf8Length(c));
        Assert.Equal("héllo", ChatText.Clamp("héllo", 100));
        Assert.Equal("h", ChatText.Clamp("hé", 2));
    }

    [Fact]
    public void CleanAndPreview()
    {
        Assert.Equal("a\nb", ChatText.Clean(" a\u0007\nb\u001b "));
        Assert.Equal("x y", ChatText.Preview("x\ny"));
        Assert.Equal(10, ChatText.Preview(new string('z', 50), 10).Length);
        Assert.Equal("a# #b", ChatText.Label("a##b"));
    }

    [Fact]
    public void TicketsAreFoundInsidePastedText()
    {
        var (kind, t) = Tickets.Find("Alice: add me! \"lpfriendafwxmtvu3li6abcdefgh\".");
        Assert.Equal(TicketKind.FriendInvite, kind);
        Assert.Equal("lpfriendafwxmtvu3li6abcdefgh", t);
        Assert.Equal(TicketKind.Room, Tickets.Find("LPROOMAFYKIA7HGVIUABCD").Kind);
        Assert.Equal(TicketKind.Announcement, Tickets.Find("lpannounceabcdefghijk").Kind);
        Assert.Equal(TicketKind.None, Tickets.Find("lpfriend").Kind);
        Assert.Equal(TicketKind.None, Tickets.Find("lpfriend0189!!!!!!!!!!").Kind);
        Assert.Equal(TicketKind.None, Tickets.Find(null).Kind);
    }
}

public class NativeJsonTests
{
    [Fact]
    public void FriendsParseAndBadInputIsEmpty()
    {
        var json = "[{\"id\":\"" + new string('a', 64) + "\",\"friend\":1,\"name\":\"Bob\\u0007\",\"online\":true,\"status\":\"away\",\"note\":\"brb\",\"last_seen\":17,\"linked\":true,\"nostr\":null}," +
                   "{\"id\":\"" + new string('b', 64) + "\",\"friend\":2,\"name\":\"\",\"online\":false,\"status\":\"invisible\",\"note\":\"\",\"last_seen\":null,\"linked\":false}]";
        var f = NativeJson.Friends(json);
        Assert.Equal(2, f.Count);
        Assert.Equal("Bob", f[0].Name);
        Assert.Equal("away", f[0].StatusLabel);
        Assert.Equal("offline", f[1].StatusLabel);
        Assert.Equal("bbbbbbbbbb", f[1].Display);
        Assert.Null(f[1].LastSeen);
        Assert.Equal(32, f[0].Key.Length);
        Assert.Empty(NativeJson.Friends("not json"));
        Assert.Empty(NativeJson.Friends("{}"));
    }

    [Fact]
    public void HistoryAnnouncementsAndSelfTest()
    {
        var h = NativeJson.History("[{\"id\":18446744073709551615,\"outgoing\":true,\"text\":\"hi\",\"sent_at\":5,\"delivered_at\":null}]");
        Assert.Equal(ulong.MaxValue, h[0].Id);
        Assert.Null(h[0].DeliveredAt);

        var a = NativeJson.AnnouncementEvent("{\"seq\":3,\"issued_at\":9,\"title\":\"T\",\"body\":\"B\"}");
        Assert.Equal(new Announcement(3, 9, "T", "B"), a);
        Assert.Null(NativeJson.AnnouncementEvent("nope"));

        var r = NativeJson.SelfTest("{\"node_id\":\"ab\",\"platform\":\"windows-x86_64\",\"bound\":[\"0.0.0.0:1\"],\"relay_url\":\"\",\"direct\":{\"ok\":true,\"ms\":1,\"detail\":\"\"},\"relay\":{\"ok\":false,\"ms\":0,\"detail\":\"relays are off\"}}");
        Assert.NotNull(r);
        Assert.True(r.Passed(relaysOn: false));
        Assert.False(r.Passed(relaysOn: true));
        var lines = r.Lines().ToList();
        Assert.Contains("direct path: ok (1 ms)", lines);
        Assert.Contains("relay path: not tried (relays are off)", lines);
    }

    [Fact]
    public void FriendLookupByNameOrPrefix()
    {
        var friends = new List<FriendView>
        {
            new(new string('a', 64), "Bob", true, "online", "", null, true, 1),
            new("ab" + new string('c', 62), "bob", false, "invisible", "", null, false, 2),
            new(new string('d', 64), "Carol", true, "online", "", null, true, 3),
        };
        Assert.Equal("Carol", FriendLookup.Find(friends, "carol").Friend?.Name);
        Assert.Null(FriendLookup.Find(friends, "bob").Friend);   // two Bobs
        Assert.Equal(2, FriendLookup.Find(friends, "abc").Friend?.FriendId);
        Assert.Contains("more than one", FriendLookup.Find(friends, "a").Error);
        Assert.Contains("no friend", FriendLookup.Find(friends, "zed").Error);
    }
}

public class ChatBookTests
{
    private static ChatLine In(ulong id, string text) => new(DateTimeOffset.UnixEpoch, "Bob", text, false, id);

    [Fact]
    public void UnreadDedupeAndStates()
    {
        var book = new ChatBook();
        var c = book.Get(ChatBook.FriendKey("aa"), "Bob");
        Assert.Same(c, book.Get(ChatBook.FriendKey("aa"), "ignored"));
        c.Add(In(1, "hi"), countUnread: true);
        c.Add(In(1, "hi"), countUnread: true);
        Assert.Single(c.Lines);
        Assert.Equal(1, book.TotalUnread);
        c.Add(new ChatLine(DateTimeOffset.UnixEpoch, "", "hey", true, 9, LineState.Pending), countUnread: true);
        Assert.Equal(1, c.Unread);
        Assert.True(c.Mark(9, LineState.Delivered));
        Assert.Equal(LineState.Delivered, c.Lines[^1].State);
        Assert.False(c.Mark(77, LineState.Failed));
        c.MarkRead();
        Assert.Equal(0, book.TotalUnread);
    }

    [Fact]
    public void BoundedAndHistoryMerges()
    {
        var c = new Conversation("c:1", "hall");
        for (int i = 0; i < Conversation.MaxLines + 20; i++)
            c.Add(new ChatLine(DateTimeOffset.UnixEpoch, "x", i.ToString(), false), countUnread: false);
        Assert.Equal(Conversation.MaxLines, c.Lines.Count);
        Assert.Equal("20", c.Lines[0].Text);

        var f = new Conversation("f:a", "Bob");
        f.Add(In(5, "live"), countUnread: false);
        f.LoadHistory([In(4, "old"), In(5, "live")]);
        Assert.Equal(["old", "live"], f.Lines.Select(l => l.Text));
    }
}

public class NotifyTests
{
    private static NotifySettings S(bool enabled = true, bool channels = false, params string[] muted) =>
        new(enabled, true, channels, false, muted.ToHashSet());

    [Fact]
    public void NothingWhileOffOrMutedOrLooking()
    {
        Assert.True(NotifyPolicy.Toast(Incoming.FriendText, "f:a", S(), null));
        Assert.False(NotifyPolicy.Toast(Incoming.FriendText, "f:a", S(enabled: false), null));
        Assert.False(NotifyPolicy.Toast(Incoming.FriendText, "f:a", S(muted: "f:a"), null));
        Assert.False(NotifyPolicy.Toast(Incoming.FriendText, "f:a", S(), "f:a"));
        Assert.False(NotifyPolicy.Toast(Incoming.ChannelLine, "c:1", S(), null), "channels are quiet by default");
        Assert.True(NotifyPolicy.Toast(Incoming.ChannelLine, "c:1", S(channels: true), null));
        Assert.False(NotifyPolicy.Toast(Incoming.FriendOnline, "f:a", S(), null));
        Assert.True(NotifyPolicy.Toast(Incoming.Announcement, "author", S(), "author"), "announcements always tell");
        Assert.False(NotifyPolicy.CountsUnread("f:a", S(muted: "f:a"), null));
        Assert.True(NotifyPolicy.CountsUnread("f:a", S(), "f:b"));
    }
}

public class IpcAndIdentityTests
{
    [Fact]
    public void PayloadsAreValidJsonAndEscaped()
    {
        using var s = JsonDocument.Parse(IpcPayloads.Status(true, true, "ab", "Bob \"B\"", 2, 3));
        Assert.Equal("Bob \"B\"", s.RootElement.GetProperty("name").GetString());
        var friends = IpcPayloads.Friends([new FriendView(new string('a', 64), "A", false, "away", "secret note", null, false, 1)]);
        using var f = JsonDocument.Parse(friends);
        Assert.Equal("offline", f.RootElement[0].GetProperty("status").GetString());
        Assert.Equal("", f.RootElement[0].GetProperty("note").GetString());
        using var m = JsonDocument.Parse(IpcPayloads.Message("friend", "ab", "A", "line\nbreak"));
        Assert.Equal("line\nbreak", m.RootElement.GetProperty("text").GetString());
    }

    [Fact]
    public void IpcNamesAreVersioned()
    {
        foreach (var f in typeof(IpcContract).GetFields(BindingFlags.Public | BindingFlags.Static))
            Assert.StartsWith("Linkpearl.v1.", (string)f.GetValue(null)!);
    }

    [Fact]
    public void TheEmbeddedAuthorIdentityIsPublicOnlyAndWellFormed()
    {
        // Empty (not configured) or exactly 64 hex characters: a 128-character value would be an ed25519
        // secret key (seed + public), which must never be embedded.
        Assert.True(AuthorIdentity.PublicKeyHex.Length is 0 or 64);
        if (AuthorIdentity.PublicKeyHex.Length == 64)
            Assert.NotNull(AuthorIdentity.Key);
        Assert.All(AuthorIdentity.NodeIdHex, n => Assert.NotNull(AuthorIdentity.ParseKey(n)));
        Assert.Null(AuthorIdentity.ParseKey("zz"));
        Assert.Null(AuthorIdentity.ParseKey(new string('g', 64)));
    }
}

public class PrivacyTests
{
    [Fact]
    public void EveryRelayChoiceIsExplained()
    {
        foreach (var c in Enum.GetValues<RelayChoice>())
            Assert.False(string.IsNullOrWhiteSpace(Privacy.Relay(c, "")));
        Assert.Contains("IP address", Privacy.Relay(RelayChoice.Default, ""));
        Assert.Contains("No third party", Privacy.Relay(RelayChoice.Off, ""));
        Assert.Contains("https://relay.example", Privacy.Relay(RelayChoice.Custom, "https://relay.example"));
    }

    private static string RepoRoot => typeof(PrivacyTests).Assembly.GetCustomAttributes<AssemblyMetadataAttribute>()
        .First(a => a.Key == "RepoRoot").Value!;

    /// <summary>
    /// "Character names are never shared unless the player types them": the plugin does not even ask the game who
    /// the player is. This fails if any of the services that would tell it are used.
    /// </summary>
    [Fact]
    public void ThePluginNeverReadsWhoThePlayerIs()
    {
        var dir = Path.Combine(RepoRoot, "src", "XivLinkpearl.Plugin");
        var sources = Directory.GetFiles(dir, "*.cs", SearchOption.AllDirectories);
        Assert.NotEmpty(sources);
        string[] banned = ["IClientState", "IObjectTable", "IPlayerState", "LocalPlayer", "LocalContentId", "IPartyList", "IChatGui.ChatMessage", "ChatMessage +="];
        foreach (var file in sources)
        {
            var text = File.ReadAllText(file);
            foreach (var b in banned)
                Assert.False(text.Contains(b, StringComparison.Ordinal), $"{Path.GetFileName(file)} uses {b}");
        }
    }
}
