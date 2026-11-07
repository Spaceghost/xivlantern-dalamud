// Drives liblantern.so through the P/Invoke binding: two nodes in one
// process, relays off, befriend and exchange a text. Proves the struct layouts
// and export signatures in Native.cs match the library, headless, on Linux.
// Skipped when LANTERN_NATIVE_DIR does not point at a built library.

using System.Diagnostics;
using System.Text;
using XivLantern.Core;

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

    private LanternNode Open(string name, uint eventCapacity = 0)
    {
        var node = LanternNode.Open(new LanternOptions
        {
            NativeDirectory = NativeDir!,
            DatabasePath = Path.Combine(dir, name + ".sqlite"),
            AppId = "lantern-cs-test/1",
            Relay = RelayMode.Disabled,
            EventQueueCapacity = eventCapacity,
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
    public void ActualNativeCacheCanBeClearedAfterCloseWithoutLosingIdentityOrDownloads()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        var database = Path.Combine(dir, "lantern.sqlite");
        var options = new LanternOptions { NativeDirectory = NativeDir!, DatabasePath = database,
            AppId = "lantern-cs-cache-test/1", Relay = RelayMode.Disabled };
        byte[] identity;
        using (var native = LanternNode.Open(options))
        {
            identity = native.NodeId();
            var blob = native.BlobAddBytes(new byte[65537], "clear.png");
            Wait(native, "cache prepared", e => e.Kind == LanternEventKind.BlobDone && e.Handle == blob);
            SharedFileCache.CheckBudget(dir);
        }
        var saved = Path.Combine(dir, "downloads", "saved.download");
        Directory.CreateDirectory(Path.GetDirectoryName(saved)!); File.WriteAllText(saved, "keep saved bytes");
        Assert.True(SharedFileCache.Clear(dir) > 0);
        Assert.False(Directory.Exists(Path.Combine(dir, "lantern.blobs")));
        Assert.Equal("keep saved bytes", File.ReadAllText(saved));
        using var reopened = LanternNode.Open(options);
        Assert.Equal(identity, reopened.NodeId());
    }

    [Fact]
    public void FriendFileExportsTransferOnlyAfterExplicitFetchOrFailCleanlyOnOldLibraries()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var alice = Open("DirectFileAlice");
        if (!alice.SupportsFriendFiles)
        {
            Assert.Throws<NotSupportedException>(() => alice.FriendFileOffer(alice.NodeId(), 1));
            Assert.Throws<NotSupportedException>(() => alice.FriendFileWithdraw(alice.NodeId(), 1));
            Assert.Equal(32, alice.NodeId().Length);
            return;
        }
        using var bob = Open("DirectFileBob");
        var accept = bob.InviteAccept(alice.InviteCreate());
        Wait(bob, "direct file friendship", e => e.Kind == LanternEventKind.FriendAdded && e.Handle == accept);
        Wait(alice, "direct file friendship", e => e.Kind == LanternEventKind.FriendAdded);
        var payload = Enumerable.Range(0, 65537).Select(i => (byte)(i % 251)).ToArray();
        var local = alice.BlobAddBytes(payload, "friend.png");
        Wait(alice, "direct file hashed", e => e.Kind == LanternEventKind.BlobDone && e.Handle == local);
        alice.FriendFileOffer(bob.NodeId(), local);
        var received = Wait(bob, "direct file offered", e => e.Kind == LanternEventKind.BlobOffer);
        Assert.Equal(alice.NodeId(), received.Peer);
        var model = new FileOffers();
        Assert.True(model.Add(received.Handle, received.PeerHex, received.Text, r => r == 0, p => p == alice.NodeIdText()));
        Assert.Equal(0UL, Assert.Single(model.All).Room);
        var destination = model.BeginFetch(received.Handle, dir, _ => true)!;
        Assert.False(File.Exists(destination));
        bob.BlobFetchBounded(received.Handle, destination);
        var result = Wait(bob, "direct file downloaded", e => e.Handle == received.Handle
            && e.Kind is LanternEventKind.BlobDone or LanternEventKind.BlobFailed);
        Assert.Equal(LanternEventKind.BlobDone, result.Kind);
        Assert.Equal(payload, File.ReadAllBytes(destination));
        alice.FriendFileWithdraw(bob.NodeId(), local);
        Wait(bob, "direct file withdrawn", e => e.Kind == LanternEventKind.BlobWithdrawn && e.Handle == received.Handle);
        Assert.Equal(payload, File.ReadAllBytes(destination));
        Assert.Throws<LanternException>(() => bob.BlobFetchBounded(received.Handle, Path.Combine(dir, "withdrawn-friend.download")));
    }

    [Fact]
    public void BoundedFileFetchCrossesTheAbiOrOldLibrariesFailCleanly()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var alice = Open("FileAlice");
        if (!alice.SupportsBoundedFileFetch)
        {
            Assert.Throws<NotSupportedException>(() => alice.BlobFetchBounded(1, Path.Combine(dir, "unsupported.download")));
            Assert.Throws<NotSupportedException>(() => alice.BlobCancelBounded(1));
            Assert.Equal(32, alice.NodeId().Length);
            Assert.False(File.Exists(Path.Combine(dir, "unsupported.download")));
            return;
        }
        using var bob = Open("FileBob");
        var roomAlice = alice.RoomJoin(RoomScope.Custom, "bounded-file-test");
        Wait(alice, "file room joined", e => e.Kind == LanternEventKind.RoomJoined && e.Handle == roomAlice);
        var roomBob = bob.RoomJoin(RoomScope.Custom, "", alice.RoomTicket(roomAlice));
        Wait(alice, "file peer connected", e => e.Kind == LanternEventKind.PeerJoined && e.Handle == roomAlice);
        Wait(bob, "file peer connected", e => e.Kind == LanternEventKind.PeerJoined && e.Handle == roomBob);
        var payload = Enumerable.Range(0, 65537).Select(i => (byte)(i % 251)).ToArray();
        var blob = alice.BlobAddBytes(payload, "clear.png");
        Wait(alice, "file hashed", e => e.Kind == LanternEventKind.BlobDone && e.Handle == blob);
        alice.BlobOffer(roomAlice, blob);
        var offer = Wait(bob, "file offered", e => e.Kind == LanternEventKind.BlobOffer);
        Assert.Equal(alice.NodeId(), offer.Peer);
        var parsed = new FileOffers();
        Assert.True(parsed.Add(offer.Handle, offer.PeerHex, offer.Text, r => r == roomBob, _ => true));
        var destination = parsed.BeginFetch(offer.Handle, dir, _ => true)!;
        Assert.False(File.Exists(destination), "receiving metadata and reserving UI state cannot fetch");
        bob.BlobFetchBounded(offer.Handle, destination);
        var done = Wait(bob, "bounded file saved", e => e.Handle == offer.Handle && e.Kind is LanternEventKind.BlobDone or LanternEventKind.BlobFailed);
        Assert.Equal(LanternEventKind.BlobDone, done.Kind);
        Assert.Equal(payload, File.ReadAllBytes(destination));
        File.WriteAllText(destination, "existing local file");
        bob.BlobFetchBounded(offer.Handle, destination);
        var failed = Wait(bob, "overwrite refused", e => e.Handle == offer.Handle && e.Kind is LanternEventKind.BlobDone or LanternEventKind.BlobFailed);
        Assert.Equal(LanternEventKind.BlobFailed, failed.Kind);
        Assert.Equal("existing local file", File.ReadAllText(destination));
        bob.Block(alice.NodeId());
        Assert.Throws<LanternException>(() => bob.BlobFetchBounded(offer.Handle, Path.Combine(dir, "blocked.download")));
        Assert.Throws<LanternException>(() => bob.BlobCancelBounded(ulong.MaxValue));
        Assert.False(File.Exists(Path.Combine(dir, "blocked.download")));
        Assert.True(bob.Unblock(alice.NodeId()));
        alice.BlobUnoffer(roomAlice, blob);
        Wait(bob, "withdrawal crosses ABI", e => e.Kind == LanternEventKind.BlobWithdrawn && e.Handle == offer.Handle);
        Assert.Throws<LanternException>(() => bob.BlobFetchBounded(offer.Handle, Path.Combine(dir, "withdrawn.download")));
        alice.BlobOffer(roomAlice, blob);
        var fresh = Wait(bob, "same file reoffered", e => e.Kind == LanternEventKind.BlobOffer);
        Assert.NotEqual(offer.Handle, fresh.Handle);
        bob.RoomLeave(roomBob);
        Wait(bob, "file channel left", e => e.Kind == LanternEventKind.RoomLeft && e.Handle == roomBob);
        Assert.Throws<LanternException>(() => bob.BlobFetchBounded(fresh.Handle, Path.Combine(dir, "left.download")));
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
    public void ActivitiesCrossTheBindingClearAndStayHiddenWhileInvisible()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var alice = Open("tag-Alice");
        using var bob = Open("tag-Bob");
        var aliceId = alice.NodeIdText();
        Assert.True(alice.SupportsActivityTags, "Tag verification requires the rebuilt native library.");
        bob.InviteAccept(alice.InviteCreate());
        Wait(bob, "Bob adds Alice", e => e.Kind == LanternEventKind.FriendAdded);
        Wait(alice, "Alice adds Bob", e => e.Kind == LanternEventKind.FriendAdded);

        void AwaitTags(string[] expected, bool online)
        {
            var clock = Stopwatch.StartNew();
            while (clock.Elapsed < TimeSpan.FromSeconds(20))
            {
                alice.Poll(); bob.Poll();
                var friend = NativeJson.Friends(bob.FriendsJson()).FirstOrDefault(f => f.Id == aliceId);
                if (friend != null && friend.Online == online && friend.Tags.SequenceEqual(expected)) return;
                Thread.Sleep(10);
            }
            throw new TimeoutException("Authenticated activity snapshot did not match.");
        }

        alice.SetActivityTags(ActivityTags.Normalize([" Maps ", "maps", "custom activity", "\0chat\n"]));
        AwaitTags(["Maps", "custom activity", "chat"], true);
        alice.SetActivityTags([]);
        AwaitTags([], true);
        alice.SetActivityTags(["maps"]);
        AwaitTags(["maps"], true);
        alice.SetPresence(PresenceStatus.Invisible, "private");
        AwaitTags([], false);
        alice.SetPresence(PresenceStatus.Online, "");
        AwaitTags(["maps"], true);
    }

    [Fact]
    public void PresetAndIdleTransitionsReachAuthenticatedFriend()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var alice = Open("PresetAlice");
        using var bob = Open("PresetBob");
        bob.InviteAccept(alice.InviteCreate());
        Wait(bob, "bob accepts alice", e => e.Kind == LanternEventKind.FriendAdded);
        Wait(alice, "alice accepts bob", e => e.Kind == LanternEventKind.FriendAdded);

        var presets = StatusPresets.Save([], "raid", 3, "prog, back at 23:00", out var error);
        Assert.Empty(error);
        var preset = StatusPresets.Find(presets, "RAID")!;
        alice.SetPresence((PresenceStatus)preset.Presence, preset.Note);
        var busy = Wait(bob, "bob sees raid preset", e => e.Kind == LanternEventKind.FriendOnline && e.Status == PresenceStatus.Busy);
        Assert.Equal(preset.Note, busy.Text);
        Assert.Equal(alice.NodeId(), busy.Peer);

        alice.SetPresence(PresenceStatus.Online, "maps?");
        Wait(bob, "bob sees online", e => e.Kind == LanternEventKind.FriendOnline && e.Status == PresenceStatus.Online && e.Text == "maps?");
        var idle = new IdleAway();
        bool Publish(uint presence, string note) { alice.SetPresence((PresenceStatus)presence, note); return true; }
        idle.Tick(true, true, 1, "maps?", 1000, 1000, Publish);
        var away = Wait(bob, "bob sees idle away", e => e.Kind == LanternEventKind.FriendOnline && e.Status == PresenceStatus.Away);
        Assert.Equal("maps?", away.Text);
        idle.Tick(true, true, 1, "maps?", 0, 1000, Publish);
        var online = Wait(bob, "bob sees restored presence", e => e.Kind == LanternEventKind.FriendOnline && e.Status == PresenceStatus.Online && e.Text == "maps?");
        Assert.Equal(alice.NodeId(), online.Peer);
        Assert.False(idle.Active);
    }

    [Fact]
    public void ChannelPullEnvelopeCrossesNativeBindingAndStartsReceiverTimer()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var alice = Open("PullAlice");
        using var bob = Open("PullBob");
        Wait(alice, "alice ready", e => e.Kind == LanternEventKind.Ready);
        Wait(bob, "bob ready", e => e.Kind == LanternEventKind.Ready);
        var roomAlice = alice.RoomJoin(RoomScope.Custom, "static");
        Wait(alice, "alice room joined", e => e.Kind == LanternEventKind.RoomJoined && e.Handle == roomAlice);
        var roomBob = bob.RoomJoin(RoomScope.Custom, "", alice.RoomTicket(roomAlice));
        Wait(alice, "alice sees bob", e => e.Kind == LanternEventKind.PeerJoined && e.Handle == roomAlice);
        Wait(bob, "bob sees alice", e => e.Kind == LanternEventKind.PeerJoined && e.Handle == roomBob);

        var encoded = PullEnvelope.Encode(10);
        alice.RoomSend(roomAlice, encoded);
        var received = Wait(bob, "bob receives countdown", e => e.Kind == LanternEventKind.Message && e.Handle == roomBob);
        Assert.Equal(alice.NodeId(), received.Peer);
        Assert.Equal(encoded, Encoding.UTF8.GetString(received.Data));
        var seconds = PullEnvelope.Parse(Encoding.UTF8.GetString(received.Data));
        Assert.Equal(10, seconds);
        var timer = new PullCountdown();
        Assert.True(timer.Start(seconds!.Value, "c:static", "Static", 1000));
        Assert.Equal(10, timer.Remaining(1000));
        Assert.True(timer.Advance(11000));
        Assert.False(timer.Advance(11001));
    }

    [Fact]
    public void FullSignedBacklogCrossesTheBindingWithASmallGeneralQueue()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var node = Open("HistoryPressure", 16);
        Assert.True(node.SupportsChannelHistory);
        Wait(node, "pressure node ready", e => e.Kind == LanternEventKind.Ready);
        var room = node.RoomJoin(RoomScope.Custom, "ffi-history-pressure");
        Wait(node, "pressure room joined", e => e.Kind == LanternEventKind.RoomJoined && e.Handle == room);
        for (int i = 0; i < 256; i++) node.RoomSendWithHistory(room, $"pressure line {i}");
        var originals = new Dictionary<string, byte[]>();
        for (int i = 0; i < 256; i++)
        {
            var live = Wait(node, "pressure live record", e => e.Kind == LanternEventKind.ChannelRecord && e.Handle == room);
            var record = ChannelRecord.FromNativeEvent(live)!;
            Assert.False(record.Historical);
            Assert.Equal(node.NodeIdText(), record.Author);
            Assert.True(originals.TryAdd(record.Signature, live.Data));
        }
        node.RoomHistoryEnable(room, true);
        var received = new HashSet<string>();
        var conversation = new ChatBook().Get("pressure", "Pressure");
        for (int i = 0; i < 256; i++)
        {
            var historical = Wait(node, "pressure saved record", e => e.Kind == LanternEventKind.ChannelHistory && e.Handle == room);
            var record = ChannelRecord.FromNativeEvent(historical)!;
            Assert.True(record.Historical);
            Assert.True(received.Add(record.Signature));
            Assert.Equal(originals[record.Signature], historical.Data);
            conversation.LoadHistory([new ChatLine(record.ClaimedTime!.Value, record.Author, record.Text,
                false, Verified: true, HistorySignature: record.Signature, ClaimedAt: record.ClaimedAt)]);
        }
        Assert.Equal(256, conversation.Lines.Count);
        Assert.Equal(0, conversation.Unread);
        Assert.DoesNotContain(Backlog[node], e => e.Kind is LanternEventKind.Message or LanternEventKind.ChannelRecord);
    }

    [Fact]
    public void LateJoinGetsOriginalSignedLinesFromAnotherMemberAfterAuthorCloses()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var author = Open("HistoryAuthor");
        using var member = Open("HistoryMember");
        using var late = Open("HistoryLate");
        Assert.True(author.SupportsChannelHistory, "History verification requires the rebuilt native library.");
        Wait(author, "author ready", e => e.Kind == LanternEventKind.Ready);
        Wait(member, "member ready", e => e.Kind == LanternEventKind.Ready);
        Wait(late, "late ready", e => e.Kind == LanternEventKind.Ready);
        var authorRoom = author.RoomJoin(RoomScope.Custom, "history-fixture");
        Wait(author, "author room", e => e.Kind == LanternEventKind.RoomJoined);
        var originalTicket = author.RoomTicket(authorRoom);
        var memberRoom = member.RoomJoin(RoomScope.Custom, "", originalTicket);
        Wait(member, "member connected", e => e.Kind == LanternEventKind.PeerJoined);
        Wait(author, "author connected", e => e.Kind == LanternEventKind.PeerJoined);
        member.RoomHistoryEnable(memberRoom, true);
        var originals = new List<LanternEvent>();
        for (int i = 0; i < 2; i++)
        {
            author.RoomSendWithHistory(authorRoom, "same signed text");
            var own = Wait(author, "signed own line", e => e.Kind == LanternEventKind.ChannelRecord);
            var received = Wait(member, "member signed line", e => e.Kind == LanternEventKind.ChannelRecord);
            Assert.Equal(own.Data, received.Data);
            originals.Add(own);
        }
        var authorId = author.NodeIdText();
        var memberTicket = member.RoomTicket(memberRoom);
        author.Dispose();
        var lateRoom = late.RoomJoin(RoomScope.Custom, "", memberTicket);
        Wait(late, "late connects to member", e => e.Kind == LanternEventKind.PeerJoined);
        late.RoomHistoryEnable(lateRoom, true);
        var book = new ChatBook();
        var conversation = book.Get("history-fixture", "History fixture");
        var signatures = new HashSet<string>();
        for (int i = 0; i < 2; i++)
        {
            var historical = Wait(late, "late historical line", e => e.Kind == LanternEventKind.ChannelHistory);
            Assert.Equal(lateRoom, historical.Handle);
            Assert.Contains(originals, e => e.Data.SequenceEqual(historical.Data));
            var record = ChannelRecord.FromNativeEvent(historical)!;
            Assert.True(record.Historical);
            Assert.Equal(authorId, record.Author);
            Assert.Equal("same signed text", record.Text);
            Assert.True(signatures.Add(record.Signature));
            conversation.LoadHistory([new ChatLine(record.ClaimedTime!.Value, record.Author, record.Text,
                false, Verified: true, HistorySignature: record.Signature, ClaimedAt: record.ClaimedAt)]);
        }
        Assert.Equal(2, conversation.Lines.Count);
        Assert.Equal(0, conversation.Unread);
        Assert.DoesNotContain(Backlog[late], e => e.Kind is LanternEventKind.Message or LanternEventKind.ChannelRecord);
        Wait(late, "provider cached", e => e.Kind == LanternEventKind.Log
            && e.Text == "channel catch-up stored: 2 new verified records");
        late.RoomHistoryClear(lateRoom);
        var cleared = Wait(late, "native clear acknowledgement", e => e.Kind == LanternEventKind.ChannelHistoryCleared);
        Assert.Equal(lateRoom, cleared.Handle);
        Assert.Empty(cleared.Data);
        conversation.Clear();
        Assert.Empty(conversation.Lines);
        late.RoomLeave(lateRoom);
        Wait(late, "late leaves", e => e.Kind == LanternEventKind.RoomLeft && e.Handle == lateRoom);
        var lateId = late.NodeIdText();
        late.Dispose();
        member.RoomSendWithHistory(memberRoom, "message while late member is away");
        var third = Wait(member, "member signed third line", e => e.Kind == LanternEventKind.ChannelRecord);
        using var restarted = Open("HistoryLate");
        Assert.Equal(lateId, restarted.NodeIdText());
        Wait(restarted, "restarted ready", e => e.Kind == LanternEventKind.Ready);
        var rejoined = restarted.RoomJoin(RoomScope.Custom, "", originalTicket);
        Wait(restarted, "late reconnects", e => e.Kind == LanternEventKind.PeerJoined && e.Handle == rejoined);
        restarted.RoomHistoryEnable(rejoined, true);
        var expected = originals.Append(third).ToList();
        var rejoinedSignatures = new HashSet<string>();
        for (int i = 0; i < 3; i++)
        {
            var historical = Wait(restarted, "rejoined historical line", e => e.Kind == LanternEventKind.ChannelHistory && e.Handle == rejoined);
            Assert.Contains(expected, e => e.Data.SequenceEqual(historical.Data));
            Assert.True(rejoinedSignatures.Add(ChannelRecord.FromNativeEvent(historical)!.Signature));
        }
        Assert.DoesNotContain(Backlog[restarted], e => e.Kind is LanternEventKind.Message or LanternEventKind.ChannelRecord);
    }

    [Fact]
    public void SealedHistoryUsesCachedProviderAfterCreatorClosesAndRecipientRestarts()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var author = Open("SealedHistoryAuthor");
        using var member = Open("SealedHistoryMember");
        using var late = Open("SealedHistoryLate");
        Assert.SkipWhen(!author.SupportsSealedChannels, "Current sealed native library required.");
        Assert.True(author.SupportsChannelHistory, "History verification requires the rebuilt native library.");
        Wait(author, "author ready", e => e.Kind == LanternEventKind.Ready);
        Wait(member, "member ready", e => e.Kind == LanternEventKind.Ready);
        Wait(late, "late ready", e => e.Kind == LanternEventKind.Ready);
        var authorRoom = author.ChannelCreateSealed("sealed history fixture");
        Wait(author, "author room", e => e.Kind == LanternEventKind.RoomJoined);
        var originalTicket = author.RoomTicket(authorRoom);
        Assert.StartsWith("ltsealed", originalTicket);
        var memberRoom = member.RoomJoin(RoomScope.Custom, "", originalTicket);
        Wait(member, "member connected", e => e.Kind == LanternEventKind.PeerJoined);
        Wait(author, "author connected", e => e.Kind == LanternEventKind.PeerJoined);
        member.RoomHistoryEnable(memberRoom, true);
        var originals = new List<LanternEvent>();
        for (int i = 0; i < 2; i++)
        {
            author.RoomSendWithHistory(authorRoom, "same signed text");
            var own = Wait(author, "signed own line", e => e.Kind == LanternEventKind.ChannelRecord);
            var received = Wait(member, "member signed line", e => e.Kind == LanternEventKind.ChannelRecord);
            Assert.Equal(own.Data, received.Data);
            originals.Add(own);
        }
        var authorId = author.NodeIdText();
        var memberTicket = member.RoomTicket(memberRoom);
        author.Dispose();
        var lateRoom = late.RoomJoin(RoomScope.Custom, "", memberTicket);
        Wait(late, "late connects to member", e => e.Kind == LanternEventKind.PeerJoined);
        late.RoomHistoryEnable(lateRoom, true);
        var book = new ChatBook();
        var conversation = book.Get("history-fixture", "History fixture");
        var signatures = new HashSet<string>();
        for (int i = 0; i < 2; i++)
        {
            var historical = Wait(late, "late historical line", e => e.Kind == LanternEventKind.ChannelHistory);
            Assert.Equal(lateRoom, historical.Handle);
            Assert.Contains(originals, e => e.Data.SequenceEqual(historical.Data));
            var record = ChannelRecord.FromNativeEvent(historical)!;
            Assert.True(record.Historical);
            Assert.Equal(authorId, record.Author);
            Assert.Equal("same signed text", record.Text);
            Assert.True(signatures.Add(record.Signature));
            conversation.LoadHistory([new ChatLine(record.ClaimedTime!.Value, record.Author, record.Text,
                false, Verified: true, HistorySignature: record.Signature, ClaimedAt: record.ClaimedAt)]);
        }
        Assert.Equal(2, conversation.Lines.Count);
        Assert.Equal(0, conversation.Unread);
        Assert.DoesNotContain(Backlog[late], e => e.Kind is LanternEventKind.Message or LanternEventKind.ChannelRecord);
        Wait(late, "provider cached", e => e.Kind == LanternEventKind.Log
            && e.Text == "channel catch-up stored: 2 new verified records");
        late.RoomHistoryClear(lateRoom);
        var cleared = Wait(late, "native clear acknowledgement", e => e.Kind == LanternEventKind.ChannelHistoryCleared);
        Assert.Equal(lateRoom, cleared.Handle);
        Assert.Empty(cleared.Data);
        conversation.Clear();
        Assert.Empty(conversation.Lines);
        late.RoomLeave(lateRoom);
        Wait(late, "late leaves", e => e.Kind == LanternEventKind.RoomLeft && e.Handle == lateRoom);
        var lateId = late.NodeIdText();
        late.Dispose();
        member.RoomSendWithHistory(memberRoom, "message while late member is away");
        var third = Wait(member, "member signed third line", e => e.Kind == LanternEventKind.ChannelRecord);
        using var restarted = Open("SealedHistoryLate");
        Assert.Equal(lateId, restarted.NodeIdText());
        Wait(restarted, "restarted ready", e => e.Kind == LanternEventKind.Ready);
        var rejoined = restarted.RoomJoin(RoomScope.Custom, "", originalTicket);
        Assert.True(restarted.RoomIsSealed(rejoined));
        Wait(restarted, "late reconnects", e => e.Kind == LanternEventKind.PeerJoined && e.Handle == rejoined);
        restarted.RoomHistoryEnable(rejoined, true);
        var expected = originals.Append(third).ToList();
        var rejoinedSignatures = new HashSet<string>();
        for (int i = 0; i < 3; i++)
        {
            var historical = Wait(restarted, "rejoined historical line", e => e.Kind == LanternEventKind.ChannelHistory && e.Handle == rejoined);
            Assert.Contains(expected, e => e.Data.SequenceEqual(historical.Data));
            Assert.True(rejoinedSignatures.Add(ChannelRecord.FromNativeEvent(historical)!.Signature));
        }
        Assert.DoesNotContain(Backlog[restarted], e => e.Kind is LanternEventKind.Message or LanternEventKind.ChannelRecord);
    }

    [Fact]
    public void NameCardIdentityPersistsOrOptionalCallsFailCleanly()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var node = Open("NameCardIdentity");
        if (!node.SupportsNostrInvites)
        {
            Assert.Throws<LanternException>(() => node.NostrIdentity());
            Assert.Throws<LanternException>(() => node.NostrSendInvite(["ws://127.0.0.1:1"], "invalid"));
            Assert.Throws<LanternException>(() => node.NostrCheckInbox(["ws://127.0.0.1:1"]));
            Assert.NotEmpty(node.NodeIdText());
            return;
        }
        // No relay is contacted: identity is local and only the public npub is read.
        var identity = node.NostrIdentity();
        Assert.StartsWith("npub1", identity);
        Assert.Equal(identity, node.NostrIdentity());
        var device = node.NodeIdText();
        node.Dispose();
        using var reopened = Open("NameCardIdentity");
        Assert.Equal(device, reopened.NodeIdText());
        Assert.Equal(identity, reopened.NostrIdentity());
    }

    [Fact]
    public void SealedChannelsAndEncryptedFilesThroughOptionalBinding()
    {
        Assert.SkipWhen(string.IsNullOrEmpty(NativeDir), "LANTERN_NATIVE_DIR is not set");
        using var alice = Open("SealedAlice");
        if (!alice.SupportsSealedChannels)
        {
            Assert.Throws<NotSupportedException>(() => alice.ChannelCreateSealed("private"));
            Assert.Throws<NotSupportedException>(() => alice.ChannelReseal(1));
            Assert.Throws<NotSupportedException>(() => alice.RoomIsSealed(1));
            Assert.Throws<NotSupportedException>(() => alice.BlobAddSealedBytes(1, [], "file"));
            return;
        }
        using var bob = Open("SealedBob");
        ulong roomAlice = alice.ChannelCreateSealed("private");
        Wait(alice, "sealed creator joined", e => e.Kind == LanternEventKind.RoomJoined && e.Handle == roomAlice);
        Assert.True(alice.RoomIsSealed(roomAlice));
        string ticket = alice.RoomTicket(roomAlice);
        Assert.StartsWith("ltsealed", ticket);
        Assert.Throws<LanternException>(() => bob.RoomJoin(RoomScope.Party, "", ticket));
        ulong roomBob = bob.RoomJoin(RoomScope.Custom, "", ticket);
        Wait(bob, "sealed peer joined", e => e.Kind == LanternEventKind.PeerJoined && e.Handle == roomBob);
        Assert.True(bob.RoomIsSealed(roomBob));
        byte[] payload = Enumerable.Range(0, 65537).Select(i => (byte)(i % 251)).ToArray();
        ulong blob = alice.BlobAddSealedBytes(roomAlice, payload, "private.bin");
        Wait(alice, "sealed file prepared", e => e.Kind == LanternEventKind.BlobDone && e.Handle == blob);
        alice.BlobOffer(roomAlice, blob);
        var offer = Wait(bob, "sealed file offered", e => e.Kind == LanternEventKind.BlobOffer);
        string path = Path.Combine(dir, "sealed-managed.download");
        Assert.False(File.Exists(path));
        Assert.Throws<LanternException>(() => bob.BlobFetch(offer.Handle, path));
        bob.BlobFetchBounded(offer.Handle, path);
        Wait(bob, "sealed file decrypted", e => e.Kind == LanternEventKind.BlobDone && e.Handle == offer.Handle);
        Assert.Equal(payload, File.ReadAllBytes(path));
        alice.BlobUnoffer(roomAlice, blob);
        Wait(bob, "sealed file withdrawn", e => e.Kind == LanternEventKind.BlobWithdrawn && e.Handle == offer.Handle);
        Assert.Equal(payload, File.ReadAllBytes(path));
        Assert.Throws<LanternException>(() => bob.BlobFetchBounded(offer.Handle, Path.Combine(dir, "stale-sealed.download")));
        ulong next = alice.ChannelReseal(roomAlice);
        Assert.True(alice.RoomIsSealed(next));
        Assert.NotEqual(ticket, alice.RoomTicket(next));
        Assert.Throws<LanternException>(() => alice.BlobOffer(next, blob));
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
