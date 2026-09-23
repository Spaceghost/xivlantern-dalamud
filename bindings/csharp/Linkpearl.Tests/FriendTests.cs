// Drives liblinkpearl.so through the P/Invoke binding: two nodes in one
// process, relays off, befriend and exchange a text. Proves the struct layouts
// and export signatures in Native.cs match the library, headless, on Linux.
// Skipped when LINKPEARL_NATIVE_DIR does not point at a built library.

using System.Diagnostics;

namespace Linkpearl.Tests;

public sealed class FriendTests : IDisposable
{
    private readonly string dir = Directory.CreateTempSubdirectory("linkpearl-cs-").FullName;

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

    private static string? NativeDir => Environment.GetEnvironmentVariable("LINKPEARL_NATIVE_DIR");

    private LinkpearlNode Open(string name)
    {
        var node = LinkpearlNode.Open(new LinkpearlOptions
        {
            NativeDirectory = NativeDir!,
            DatabasePath = Path.Combine(dir, name + ".sqlite"),
            AppId = "linkpearl-cs-test/1",
            Relay = RelayMode.Disabled,
        });
        node.SetDisplayName(name);
        return node;
    }

    private static readonly Dictionary<LinkpearlNode, List<LinkpearlEvent>> Backlog = new();

    private static LinkpearlEvent Wait(LinkpearlNode node, string what, Func<LinkpearlEvent, bool> match)
    {
        if (!Backlog.TryGetValue(node, out var backlog))
        {
            backlog = new List<LinkpearlEvent>();
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
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LINKPEARL_NATIVE_DIR is not set");

        using var alice = Open("Alice");
        using var bob = Open("Bob");
        byte[] aliceId = alice.NodeId();
        byte[] bobId = bob.NodeId();

        string invite = alice.InviteCreate();
        Assert.StartsWith("lpfriend", invite);
        ulong handle = bob.InviteAccept(invite);

        var added = Wait(bob, "bob adds alice", e =>
            e.Kind is LinkpearlEventKind.FriendAdded or LinkpearlEventKind.InviteFailed);
        Assert.Equal(LinkpearlEventKind.FriendAdded, added.Kind);
        Assert.Equal(handle, added.Handle);
        Assert.Equal("Alice", added.Text);
        Assert.Equal(aliceId, added.Peer);
        Wait(alice, "alice adds bob", e => e.Kind == LinkpearlEventKind.FriendAdded && e.Text == "Bob");

        ulong id = bob.FriendSend(aliceId, "hello from C#");
        var text = Wait(alice, "alice gets the text", e => e.Kind == LinkpearlEventKind.FriendText);
        Assert.Equal("hello from C#", text.Text);
        Assert.Equal(id, text.Handle);
        Assert.Equal(bobId, text.Peer);
        Wait(bob, "bob sees delivered", e => e.Kind == LinkpearlEventKind.FriendDelivered && e.Handle == id);

        alice.SetPresence(PresenceStatus.Away, "brb");
        var away = Wait(bob, "bob sees alice away", e =>
            e.Kind == LinkpearlEventKind.FriendOnline && e.Status == PresenceStatus.Away);
        Assert.Equal("brb", away.Text);

        Assert.Contains("\"name\":\"Bob\"", alice.FriendsJson());
        Assert.Contains("hello from C#", alice.FriendHistoryJson(bobId));
        Assert.Throws<ArgumentException>(() => alice.FriendSend(new byte[3], "x"));
    }
}
