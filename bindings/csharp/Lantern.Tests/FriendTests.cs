// Drives liblantern.so through the P/Invoke binding: two nodes in one
// process, relays off, befriend and exchange a text. Proves the struct layouts
// and export signatures in Native.cs match the library, headless, on Linux.
// Skipped when LANTERN_NATIVE_DIR does not point at a built library.

using System.Diagnostics;

namespace Lantern.Tests;

public sealed class FriendTests : IDisposable
{
    private readonly string dir = Directory.CreateTempSubdirectory("lantern-cs-").FullName;

    public void Dispose()
    {
        try
        {
            Directory.Delete(dir, recursive: true);
        }
        catch (IOException)
        {
        }
    }

    private static string? NativeDir => Environment.GetEnvironmentVariable("LANTERN_NATIVE_DIR");

    private LanternNode Open(string name)
    {
        var node = LanternNode.Open(new LanternOptions
        {
            NativeDirectory = NativeDir!,
            DatabasePath = Path.Combine(dir, name + ".sqlite"),
            AppId = "lantern-cs-test/1",
            Relay = RelayMode.Disabled,
        });
        node.SetDisplayName(name);
        return node;
    }

    private static readonly Dictionary<LanternNode, List<LanternEvent>> Backlog = new();

    private static LanternEvent Wait(LanternNode node, string what, Func<LanternEvent, bool> match)
    {
        if (!Backlog.TryGetValue(node, out var backlog))
        {
            backlog = new List<LanternEvent>();
            Backlog[node] = backlog;
        }

        var clock = Stopwatch.StartNew();
        while (clock.Elapsed < TimeSpan.FromSeconds(20))
        {
            backlog.AddRange(node.Poll());
            int i = backlog.FindIndex(e => match(e));
            if (i >= 0)
            {
                var hit = backlog[i];
                backlog.RemoveAt(i);
                return hit;
            }

            Thread.Sleep(10);
        }

        throw new TimeoutException($"{what}; saw: {string.Join(", ", backlog.Select(e => e.Kind))}");
    }

    [Fact]
    public void TwoNodesBefriendAndText()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");

        using var alice = Open("Alice");
        using var bob = Open("Bob");
        byte[] aliceId = alice.NodeId();
        byte[] bobId = bob.NodeId();

        string invite = alice.InviteCreate();
        Assert.StartsWith("ltfriend", invite);
        ulong handle = bob.InviteAccept(invite);

        var added = Wait(bob, "bob adds alice", e =>
            e.Kind is LanternEventKind.FriendAdded or LanternEventKind.InviteFailed);
        Assert.Equal(LanternEventKind.FriendAdded, added.Kind);
        Assert.Equal(handle, added.Handle);
        Assert.Equal("Alice", added.Text);
        Assert.Equal(aliceId, added.Peer);
        Wait(alice, "alice adds bob", e => e.Kind == LanternEventKind.FriendAdded && e.Text == "Bob");

        ulong id = bob.FriendSend(aliceId, "hello from C#");
        var text = Wait(alice, "alice gets the text", e => e.Kind == LanternEventKind.FriendText);
        Assert.Equal("hello from C#", text.Text);
        Assert.Equal(id, text.Handle);
        Assert.Equal(bobId, text.Peer);
        Wait(bob, "bob sees delivered", e => e.Kind == LanternEventKind.FriendDelivered && e.Handle == id);

        alice.SetPresence(PresenceStatus.Away, "brb");
        var away = Wait(bob, "bob sees alice away", e =>
            e.Kind == LanternEventKind.FriendOnline && e.Status == PresenceStatus.Away);
        Assert.Equal("brb", away.Text);

        Assert.Contains("\"name\":\"Bob\"", alice.FriendsJson());
        Assert.Contains("hello from C#", alice.FriendHistoryJson(bobId));
        Assert.Throws<ArgumentException>(() => alice.FriendSend(new byte[3], "x"));
    }

    [Fact]
    public void SelfTestBlockAndAuthorChannelThroughTheBinding()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");

        using var alice = Open("Alice2");
        using var bob = Open("Bob2");
        ulong handle = alice.SelfTest();
        var report = Wait(alice, "selftest", e => e.Kind == LanternEventKind.SelfTest && e.Handle == handle);
        Assert.Contains("\"direct\":{\"ok\":true", report.Text);

        byte[] bobId = bob.NodeId();
        alice.Block(bobId);
        Assert.Contains(Convert.ToHexStringLower(bobId), alice.BlockedJson());
        Assert.True(alice.Unblock(bobId));
        Assert.False(alice.Unblock(bobId));

        byte[] author = bob.NodeId(); // any valid ed25519 public key will do
        ulong room = alice.AuthorJoin(author, [bobId]);
        Assert.Equal(room, alice.AuthorJoin(author, [bobId]));
        Assert.Equal("[]", alice.AnnouncementsJson(author));
        Assert.Throws<LanternException>(() => alice.RoomSend(room, "chatter"));
        alice.SetSupportContacts([bobId]);
        alice.SetSupportContacts([]);
        Assert.Throws<LanternException>(() => alice.SupportSend(bobId, "hi"));
        Assert.Equal(0u, alice.Stats().RateLimited);
    }
}
