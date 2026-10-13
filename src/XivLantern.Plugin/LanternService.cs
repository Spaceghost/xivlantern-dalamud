using System.Collections.Concurrent;
using System.Text;
using Dalamud.Plugin.Services;
using Lantern;
using XivLantern.Core;

namespace XivLantern.Plugin;

public enum ServiceState
{
    Off,
    Starting,
    On,
    Stopping,
    Failed,
}

/// <summary>A channel this session is in.</summary>
public sealed class ChannelInfo(ulong room, string label, string ticket)
{
    public ulong Room { get; } = room;
    public string Label { get; } = label;
    /// <summary>The ticket it was saved under: the key for its conversation and its mute.</summary>
    public string Ticket { get; } = ticket;
    public bool Sealed { get; } = ticket.StartsWith("ltsealed", StringComparison.Ordinal);
    public string Key => ChatBook.ChannelKey(Ticket);
    public HashSet<string> Members { get; } = [];
}

/// <summary>A queued offer owned by this session; no local file path is exposed in the row.</summary>
public sealed record OutgoingFileOffer(ulong Handle, ulong Room, string Key, string Name, ulong Size);

/// <summary>A channel a friend invited us to. Nothing is joined until the player says so.</summary>
public sealed record ChannelInviteOffer(string FromId, string FromName, string Ticket, string Label);

/// <summary>
/// Owns the Lantern node. Opening and closing happen on a worker thread (both block); everything else — commands,
/// <c>lt_poll</c>, the chat model — happens on the framework thread, which is also the thread that draws, so the
/// model needs no locks. Nothing is loaded or connected until <see cref="Start"/>, which only runs once the player
/// has turned Lantern on.
/// </summary>
public sealed class LanternService : IDisposable
{
    public const string AppId = "xivlantern/1";
    private static readonly TimeSpan FriendRefresh = TimeSpan.FromSeconds(2);

    private readonly string nativeDir;
    private readonly string dbPath;
    private readonly Configuration config;
    private readonly Action save;
    private readonly IPluginLog log;
    private readonly IFramework framework;
    private readonly ConcurrentQueue<Action> onFramework = new();
    private readonly Dictionary<ulong, Action<SelfTestReport?, string>> selftests = [];
    private readonly Dictionary<string, string> memberNames = [];
    private readonly HashSet<string> historyLoaded = [];
    private LanternNode? node;
    /// <summary>Opens, closes and throwaway selftests still running. Unload waits for them: no Rust thread may
    /// outlive the plugin's code.</summary>
    private readonly List<Task> background = [];
    // Only workers/unload take this lock; frame callbacks never wait for preparation.
    private readonly object nostrWorkGate = new();
    private readonly object fileStorageGate = new(); // Workers only: serialize cleanup and quota scans.
    private CancellationTokenSource? nostrPreparation;
    public bool SupportsNostrInvites => node?.SupportsNostrInvites == true;
    private readonly NostrOperation nostrOperation = new();
    public bool NostrBusy => nostrOperation.Busy;
    public string NostrNpub { get; private set; } = "";
    public string NostrStatus => nostrOperation.Status;
    public string NostrResolvedName { get; private set; } = "";
    public string NostrResolvedKey { get; private set; } = "";
    public NostrInbox NameCardInbox { get; }
    private DateTime nextFriendRefresh;
    private int generation;
    private readonly IdleAway idleAway = new();
    private long nextIdlePoll;
    public uint EffectivePresence => idleAway.Active ? 2u : config.Presence;

    public LanternService(string nativeDir, string configDir, Configuration config, Action save, IPluginLog log, IFramework framework)
    {
        this.nativeDir = nativeDir;
        dbPath = Path.Combine(configDir, "lantern.sqlite");
        this.config = config;
        NameCardInbox = new NostrInbox(config.NostrIgnoredInvites);
        this.save = save;
        this.log = log;
        this.framework = framework;
        framework.Update += OnUpdate;
    }

    public ServiceState State { get; private set; } = ServiceState.Off;
    public string LastError { get; private set; } = "";
    public string NodeId { get; private set; } = "";
    public string NodeTicket { get; private set; } = "";
    public ChatBook Book { get; } = new();
    public FileOffers FileOffers { get; } = new();
    public bool SupportsSealedChannels => node?.SupportsSealedChannels == true;
    public bool SupportsFileDownloads => node?.SupportsBoundedFileFetch == true;
    public bool SupportsFriendFiles => node?.SupportsFriendFiles == true;
    public string FileDownloadStatus { get; private set; } = "";
    private readonly HashSet<ulong> queuedFileDownloads = [];
    public bool FileSendBusy { get; private set; }
    private volatile bool cacheCloseUncertain;
    public bool SharedCacheCloseError => cacheCloseUncertain;
    public bool SharedCacheBusy { get; private set; }
    public bool CanClearSharedCache
    {
        get
        {
            if (cacheCloseUncertain || State != ServiceState.Off || node != null || SharedCacheBusy || !onFramework.IsEmpty) return false;
            lock (background) return background.All(t => t.IsCompleted);
        }
    }
    public string SharedCacheStatus { get; private set; } = "";
    public string FileSendStatus { get; private set; } = "";
    private OutgoingFileOffer? filePublication;
    private readonly List<OutgoingFileOffer> outgoingFiles = [];
    public IReadOnlyList<OutgoingFileOffer> OutgoingFiles => outgoingFiles;
    private readonly Dictionary<ulong, bool> earlyFilePublication = [];
    private ulong sharedFileBytes;
    private int sharedFileCount;
    public IReadOnlyList<FriendView> Friends { get; private set; } = [];
    public List<ChannelInfo> Channels { get; } = [];
    public List<ChannelInviteOffer> ChannelInvites { get; } = [];
    public List<Announcement> Announcements { get; private set; } = [];
    public IReadOnlyList<string> Blocked { get; private set; } = [];
    public ulong? AuthorRoom { get; private set; }
    public string DataPath => dbPath;
    public bool SupportsChannelHistory => node?.SupportsChannelHistory == true;
    public bool IsOn => State == ServiceState.On && node is not null;
    public PullCountdown Pull { get; } = new();
    public event Action<string>? PullCompleted;

    /// <summary>The conversation the window is showing, so it is not toasted or counted as unread.</summary>
    public string? OpenConversation { get; set; }

    /// <summary>Raised on the framework thread: something worth a toast (or an IPC event) arrived.</summary>
    public event Action<Incoming, string, string, string>? Arrived;

    /// <summary>A line for the player: shown in the window's status area and, for commands, printed to chat.</summary>
    public event Action<string>? Notice;

    public LanternStats? Stats => node?.Stats();

    public NotifySettings NotifySettings => new(IsOn, config.Toasts, config.ChannelToasts, config.FriendOnlineToasts, config.Muted);

    // ------------------------------------------------------------ lifecycle

    public void Start()
    {
        if (SharedCacheBusy || State is ServiceState.Starting or ServiceState.On or ServiceState.Stopping)
            return;
        if (!config.PrivacyAcknowledged)
        {
            Fail("read and tick the privacy note first");
            return;
        }

        State = ServiceState.Starting;
        LastError = "";
        var options = Options(dbPath);
        int gen = ++generation;
        Background(() =>
        {
            try
            {
                var opened = LanternNode.Open(options);
                if (gen != Volatile.Read(ref generation))
                {
                    // Turned off, or unloading, while it was opening.
                    ClosePersistentNode(opened);
                    return;
                }

                onFramework.Enqueue(() => Opened(opened, gen));
            }
            catch (Exception ex)
            {
                onFramework.Enqueue(() => Fail(ex.Message));
            }
        });
    }

    private void Background(Action work)
    {
        lock (background)
        {
            background.RemoveAll(t => t.IsCompleted);
            background.Add(Task.Run(work));
        }
    }

    private LanternOptions Options(string? db) => new()
    {
        NativeDirectory = nativeDir,
        DatabasePath = db,
        AppId = AppId,
        Relay = (RelayMode)(uint)config.Relay,
        RelayUrl = config.Relay == RelayChoice.Custom ? config.CustomRelayUrl.Trim() : null,
        AcceptInbound = true,
    };

    private void Opened(LanternNode opened, int gen)
    {
        if (gen != generation || State != ServiceState.Starting)
        {
            // Turned off while it was opening.
            Background(() => ClosePersistentNode(opened));
            return;
        }

        node = opened;
        State = ServiceState.On;
        NodeId = node.NodeIdText();
        NodeTicket = node.NodeTicket();
        try
        {
            if (config.DisplayName.Trim().Length > 0)
                node.SetDisplayName(config.DisplayName.Trim());
            node.SetPresence((PresenceStatus)config.Presence, config.PresenceNote);
            if (node.SupportsActivityTags) node.SetActivityTags(config.ActivityTags);
            if (config.RejoinChannels)
            {
                foreach (var saved in config.Channels.ToList())
                    JoinSaved(saved);
            }

            if (config.JoinAuthorChannel)
                JoinAuthor();
            RefreshFriends();
            RefreshBlocked();
            Say("Lantern is on. Node id " + NodeId[..16] + "…");
        }
        catch (LanternException ex)
        {
            Say("Lantern started, but: " + ex.Message);
        }
    }

    private void ClosePersistentNode(LanternNode closing)
    {
        try { lock (nostrWorkGate) closing.Dispose(); }
        catch { cacheCloseUncertain = true; throw; }
    }

    public void Stop()
    {
        nostrPreparation?.Cancel();
        generation++;
        var n = node;
        node = null;
        nostrOperation.Reset();
        NostrNpub = "";
        NostrResolvedName = NostrResolvedKey = "";
        NameCardInbox.ClearPending();
        FileOffers.Clear();
        queuedFileDownloads.Clear();
        FileSendBusy = false;
        FileSendStatus = FileDownloadStatus = "";
        filePublication = null;
        outgoingFiles.Clear();
        earlyFilePublication.Clear();
        sharedFileBytes = 0;
        sharedFileCount = 0;
        Channels.Clear();
        AuthorRoom = null;
        Friends = [];
        historyLoaded.Clear();
        if (n is null)
        {
            State = ServiceState.Off;
            return;
        }

        State = ServiceState.Stopping;
        Background(() =>
        {
            try
            {
                ClosePersistentNode(n);
            }
            catch (Exception ex)
            {
                log.Warning(ex, "Lantern: closing the node failed");
            }

            onFramework.Enqueue(() =>
            {
                if (State == ServiceState.Stopping)
                    State = ServiceState.Off;
            });
        });
    }

    private void Fail(string message)
    {
        State = ServiceState.Failed;
        LastError = message;
        Say("Lantern could not start: " + message);
    }

    private void Say(string line)
    {
        log.Information("Lantern: {Line}", line);
        Notice?.Invoke(line);
    }

    public void Dispose()
    {
        nostrPreparation?.Cancel();
        framework.Update -= OnUpdate;
        Interlocked.Increment(ref generation);
        Task[] pending;
        lock (background)
            pending = background.ToArray();
        try
        {
            // An open finishing now sees the new generation and closes its own node.
            Task.WaitAll(pending, TimeSpan.FromSeconds(15));
        }
        catch (AggregateException)
        {
            // Each task reports its own failures.
        }

        var n = node;
        node = null;
        // Bounded (the runtime is given half a second), and it must finish before the plugin's code is unloaded:
        // no Rust thread may outlive it. The native library itself stays loaded (see Native.Load).
        try
        {
            if (n != null) ClosePersistentNode(n);
            nostrPreparation?.Dispose();
            nostrPreparation = null;
        }
        catch (Exception ex)
        {
            log.Warning(ex, "Lantern: closing the node on unload failed");
        }
    }

    // ------------------------------------------------------------ the frame

    private void OnUpdate(IFramework _)
    {
        while (onFramework.TryDequeue(out var action))
        {
            try
            {
                action();
            }
            catch (Exception ex)
            {
                log.Error(ex, "Lantern: a queued action failed");
            }
        }

        if (node is null)
        {
            Pull.Clear();
            idleAway.Tick(false, false, config.Presence, config.PresenceNote, null, 1, (_, _) => false);
            return;
        }
        try
        {
            var now = Environment.TickCount64;
            ForgetFileOffers(offer => !FileAllowed(offer));
            if (!config.PullTimersEnabled || config.Muted.Contains(Pull.Channel) || !Channels.Any(c => c.Key == Pull.Channel))
                Pull.Clear();
            else if (Pull.Advance(now)) PullCompleted?.Invoke(Pull.Label);
            if (now >= nextIdlePoll)
            {
                nextIdlePoll = now + 1000;
                idleAway.Tick(config.IdleAwayEnabled, IsOn, config.Presence, config.PresenceNote,
                    config.IdleAwayEnabled ? IdleInput.ReadMilliseconds() : null,
                    Math.Clamp(config.IdleAwayMinutes, 1, 120) * 60_000L,
                    (presence, note) => Guard(n => { n.SetPresence((PresenceStatus)presence, note); return ""; }).Length == 0);
            }
            // Bounded per frame; the rest waits for the next one.
            for (int batch = 0; batch < 4; batch++)
            {
                var events = node.Poll(64);
                foreach (var e in events)
                    Handle(e);
                if (events.Count < 64)
                    break;
            }

            if (DateTime.UtcNow >= nextFriendRefresh)
                RefreshFriends();
        }
        catch (Exception ex)
        {
            log.Error(ex, "Lantern: handling events failed");
        }
    }

    private static string Hex(byte[] id) => Convert.ToHexStringLower(id);

    public string NameOf(string id)
    {
        var f = Friends.FirstOrDefault(x => x.Id == id);
        if (f is not null && f.Name.Length > 0)
            return f.Name;
        return memberNames.TryGetValue(id, out var n) && n.Length > 0 ? n : id[..10];
    }

    private void Handle(LanternEvent e)
    {
        var peer = Hex(e.Peer);
        switch (e.Kind)
        {
            case LanternEventKind.Ready:
                NodeId = e.Text;
                break;
            case LanternEventKind.FriendAdded:
                if (NameCardInbox.Complete(e.Handle, true)) SaveNostrIgnores();
                RefreshFriends();
                FriendChat(peer).Add(System("You are now friends with " + ChatText.Clean(e.Text) + "."), false);
                Arrived?.Invoke(Incoming.InviteAccepted, ChatBook.FriendKey(peer), ChatText.Clean(e.Text), "is now your friend");
                break;
            case LanternEventKind.FriendRemoved:
                RefreshFriends();
                break;
            case LanternEventKind.InviteFailed:
                NameCardInbox.Complete(e.Handle, false);
                Say("That invite did not work: " + e.Text);
                break;
            case LanternEventKind.FriendOnline:
                bool wasOnline = Friends.FirstOrDefault(f => f.Id == peer)?.Online ?? false;
                RefreshFriends();
                if (!wasOnline)
                    Arrived?.Invoke(Incoming.FriendOnline, ChatBook.FriendKey(peer), NameOf(peer), "is online");
                break;
            case LanternEventKind.FriendOffline:
                RefreshFriends();
                break;
            case LanternEventKind.FriendText:
            {
                var key = ChatBook.FriendKey(peer);
                var text = ChatText.Clean(e.Text);
                FriendChat(peer).Add(new ChatLine(DateTimeOffset.Now, NameOf(peer), text, false, e.Handle), NotifyPolicy.CountsUnread(key, NotifySettings, OpenConversation));
                Arrived?.Invoke(Incoming.FriendText, key, NameOf(peer), text);
                break;
            }
            case LanternEventKind.FriendDelivered:
                FriendChat(peer).Mark(e.Handle, LineState.Delivered);
                break;
            case LanternEventKind.NostrDone:
            case LanternEventKind.NostrFailed:
                nostrOperation.Completed(e.Handle, ChatText.Clamp(ChatText.Clean(e.Text), 512));
                break;
            case LanternEventKind.NostrInvite:
                if (config.NostrInvitesEnabled) NameCardInbox.Add(peer, e.Text);
                break;
            case LanternEventKind.BlobOffer:
                FileOffers.Add(e.Handle, peer, e.Text,
                    room => room == 0 ? Friends.Any(f => f.Id == peer)
                        : Channel(room) is { } channel && !config.Muted.Contains(channel.Key),
                    id => !Blocked.Contains(id, StringComparer.OrdinalIgnoreCase)
                        && !config.Muted.Contains(ChatBook.FriendKey(id)));
                break;
            case LanternEventKind.BlobProgress:
                FileOffers.Progress(e.Handle, e.Progress.Done);
                break;
            case LanternEventKind.BlobWithdrawn:
                FileOffers.Withdraw(e.Handle);
                break;
            case LanternEventKind.BlobDone:
                // The bounded native path verifies content and exact size before completion.
                // Never use event text as a destination or automatically open the file.
                if (queuedFileDownloads.Remove(e.Handle)) FileOffers.Complete(e.Handle, true);
                else FilePublicationResult(e.Handle, true);
                break;
            case LanternEventKind.BlobFailed:
                if (queuedFileDownloads.Remove(e.Handle)) FileOffers.Complete(e.Handle, false);
                else FilePublicationResult(e.Handle, false);
                break;
            case LanternEventKind.ChannelInvite:
            {
                var (kind, ticket) = Tickets.Find(e.Text);
                if ((kind is TicketKind.Room or TicketKind.SealedRoom) && !ChannelInvites.Any(c => c.Ticket == ticket) && !Channels.Any(c => c.Ticket == ticket))
                {
                    ChannelInvites.Add(new ChannelInviteOffer(peer, NameOf(peer), ticket, "a channel"));
                    Arrived?.Invoke(Incoming.FriendText, ChatBook.FriendKey(peer), NameOf(peer), "invited you to a channel (open Lantern to join or ignore)");
                }

                break;
            }
            case LanternEventKind.RoomJoined when Channel(e.Handle) is { } c:
                ChannelChat(c).Add(System("Joined #" + c.Label + "."), false);
                break;
            case LanternEventKind.PeerJoined when Channel(e.Handle) is { } c:
                c.Members.Add(peer);
                break;
            case LanternEventKind.PeerLeft when Channel(e.Handle) is { } c:
                c.Members.Remove(peer);
                break;
            case LanternEventKind.Presence when Channel(e.Handle) is not null:
                // In a channel, room presence is the name a member typed.
                memberNames[peer] = ChatText.Clamp(ChatText.Clean(e.Text), ChatText.MaxName);
                break;
            case LanternEventKind.Message when Channel(e.Handle) is { } c:
            {
                var text = ChatText.Clean(Encoding.UTF8.GetString(e.Data));
                if (text.Length == 0)
                    break;
                if (PullEnvelope.Parse(text) is { } seconds && config.PullTimersEnabled && !config.Muted.Contains(c.Key))
                    Pull.Start(seconds, c.Key, c.Label, Environment.TickCount64);
                ChannelChat(c).Add(new ChatLine(DateTimeOffset.Now, NameOf(peer), ChatText.Clamp(text, ChatText.MaxChannelBytes), false),
                    NotifyPolicy.CountsUnread(c.Key, NotifySettings, OpenConversation));
                Arrived?.Invoke(Incoming.ChannelLine, c.Key, NameOf(peer) + " in #" + c.Label, text);
                break;
            }
            case LanternEventKind.ChannelRecord or LanternEventKind.ChannelHistory when Channel(e.Handle) is { } c:
            {
                var record = Lantern.ChannelRecord.FromNativeEvent(e);
                if (record is null) break;
                var outgoing = record.Author == NodeId;
                var line = new ChatLine(record.ClaimedTime ?? DateTimeOffset.Now, outgoing ? "" : NameOf(record.Author),
                    record.Text, outgoing, Verified: true, HistorySignature: record.Signature, ClaimedAt: record.ClaimedAt, SignedAuthor: record.Author);
                if (record.Historical) ChannelChat(c).LoadHistory([line]);
                else
                {
                    ChannelChat(c).Add(line, !outgoing && NotifyPolicy.CountsUnread(c.Key, NotifySettings, OpenConversation));
                    if (!outgoing)
                    {
                        if (PullEnvelope.Parse(record.Text) is { } seconds && config.PullTimersEnabled && !config.Muted.Contains(c.Key))
                            Pull.Start(seconds, c.Key, c.Label, Environment.TickCount64);
                        Arrived?.Invoke(Incoming.ChannelLine, c.Key, NameOf(record.Author) + " in #" + c.Label, record.Text);
                    }
                }
                break;
            }
            case LanternEventKind.ChannelHistoryCleared when Channel(e.Handle) is { } c:
                ChannelChat(c).Clear();
                break;
            case LanternEventKind.RateLimited:
                Say($"{NameOf(peer)} is sending too fast; some of their {e.Text} messages were dropped.");
                break;
            case LanternEventKind.Announcement:
                if (NativeJson.AnnouncementEvent(e.Text) is { } a)
                {
                    Announcements.RemoveAll(x => x.Seq == a.Seq);
                    Announcements.Insert(0, a);
                    AuthorChat().Add(new ChatLine(DateTimeOffset.FromUnixTimeSeconds(a.IssuedAt), "Author", a.Title + (a.Body.Length > 0 ? "\n" + a.Body : ""), false, a.Seq, Verified: true),
                        NotifyPolicy.CountsUnread(ChatBook.AuthorKey, NotifySettings, OpenConversation));
                    Arrived?.Invoke(Incoming.Announcement, ChatBook.AuthorKey, "Author (verified)", a.Title);
                }

                break;
            case LanternEventKind.SupportReply:
            {
                var text = ChatText.Clean(e.Text);
                SupportChat().Add(new ChatLine(DateTimeOffset.Now, "Author", text, false, e.Handle, Verified: true),
                    NotifyPolicy.CountsUnread(ChatBook.SupportKey, NotifySettings, OpenConversation));
                Arrived?.Invoke(Incoming.SupportReply, ChatBook.SupportKey, "Author", text);
                break;
            }
            case LanternEventKind.SupportDelivered:
                SupportChat().Mark(e.Handle, LineState.Delivered);
                break;
            case LanternEventKind.SupportFailed:
                SupportChat().Mark(e.Handle, LineState.Failed);
                SupportChat().Add(System("Not delivered: " + e.Text), false);
                break;
            case LanternEventKind.SelfTest:
                if (selftests.Remove(e.Handle, out var done))
                    done(NativeJson.SelfTest(e.Text), e.Text);
                break;
            case LanternEventKind.Error:
                log.Warning("Lantern: {Message}", e.Text);
                break;
            case LanternEventKind.Log:
                log.Debug("Lantern: {Message}", e.Text);
                break;
        }
    }

    private static ChatLine System(string text) => new(DateTimeOffset.Now, "", text, false, System: true);

    private ChannelInfo? Channel(ulong room) => Channels.FirstOrDefault(c => c.Room == room);

    public Conversation FriendChat(string id) => Book.Get(ChatBook.FriendKey(id), NameOf(id));
    public Conversation ChannelChat(ChannelInfo c) => Book.Get(c.Key, "#" + c.Label);
    public Conversation AuthorChat() => Book.Get(ChatBook.AuthorKey, "Announcements");
    public Conversation SupportChat() => Book.Get(ChatBook.SupportKey, "Write to the author");

    public void RefreshFriends()
    {
        nextFriendRefresh = DateTime.UtcNow + FriendRefresh;
        if (node is null)
            return;
        Friends = NativeJson.Friends(node.FriendsJson())
            .OrderByDescending(f => f.Online)
            .ThenBy(f => f.Display, StringComparer.OrdinalIgnoreCase)
            .ToList();
        foreach (var f in Friends)
        {
            if (Book.Find(ChatBook.FriendKey(f.Id)) is { } c)
                c.Title = f.Display;
        }
    }

    private void RefreshBlocked()
    {
        if (node is not null)
            Blocked = NativeJson.Strings(node.BlockedJson());
    }

    // ------------------------------------------------------------- actions
    // Each returns "" on success or a sentence for the player.

    private void SaveNostrIgnores()
    {
        config.NostrIgnoredInvites = NameCardInbox.IgnoredKeys.ToList();
        save();
    }

    public string AcceptNostrInvite(string key) => Guard(n =>
    {
        if (!config.NostrInvitesEnabled) return "Name-card invites are disabled.";
        var offer = NameCardInbox.Find(key);
        if (offer is null) return "That invitation is no longer pending.";
        if (offer.AcceptHandle != 0) return "Already asking them…";
        NameCardInbox.AcceptStarted(key, n.InviteAccept(offer.Ticket));
        return "";
    });

    public void IgnoreNostrInvite(string key)
    {
        if (NameCardInbox.Ignore(key)) SaveNostrIgnores();
    }

    public void SetNostrInvitesEnabled(bool enabled)
    {
        config.NostrInvitesEnabled = enabled;
        if (!enabled)
        {
            nostrPreparation?.Cancel();
            NameCardInbox.ClearPending();
            NostrResolvedName = NostrResolvedKey = "";
        }
        save();
    }

    private string PrepareNostr(Func<LanternNode, string[], CancellationToken, (string Status, ulong Job)> prepare)
    {
        var n = node;
        if (n is null) return "Lantern is off.";
        if (!config.NostrInvitesEnabled) return "Enable name-card invites first.";
        if (!n.SupportsNostrInvites) return "This Lantern build does not include name-card invites.";
        if (NostrBusy) return "Name-card preparation is already running.";
        var relays = (config.NostrRelays ?? []).ToArray();
        var gen = generation;
        nostrPreparation?.Dispose();
        var cancellation = new CancellationTokenSource();
        nostrPreparation = cancellation;
        nostrOperation.Begin();
        Background(() =>
        {
            (string Status, ulong Job) result;
            lock (nostrWorkGate)
            {
                if (gen != Volatile.Read(ref generation)) return;
                try
                {
                    cancellation.Token.ThrowIfCancellationRequested();
                    result = prepare(n, relays, cancellation.Token);
                }
                catch (OperationCanceledException) { result = ("Name-card preparation canceled.", 0); }
                catch (Exception ex) { result = (ex is LanternException or ArgumentException
                    ? ex.Message : "Name-card preparation failed.", 0); }
            }
            onFramework.Enqueue(() =>
            {
                if (gen != generation || node != n) return;
                nostrOperation.Prepared(result.Status, result.Job);
            });
        });
        return "";
    }

    public string PrepareNostrIdentity() => PrepareNostr((n, _, cancellation) =>
    {
        var npub = n.NostrIdentity();
        var gen = Volatile.Read(ref generation);
        onFramework.Enqueue(() => { if (gen == generation && node == n) NostrNpub = npub; });
        return ("Name card ready.", 0UL);
    });

    public string ResolveNostrName(string identifier)
    {
        if (NostrBusy) return "Name-card preparation is already running.";
        NostrResolvedName = NostrResolvedKey = "";
        return PrepareNostr((n, _, cancellation) =>
        {
            var gen = Volatile.Read(ref generation);
            using var handler = new HttpClientHandler { AllowAutoRedirect = false, UseCookies = false };
            using var client = new HttpClient(handler);
            var key = NameCardLookup.Resolve(client, identifier, cancellation).GetAwaiter().GetResult();
            onFramework.Enqueue(() =>
            {
                if (gen != generation || node != n || cancellation.IsCancellationRequested) return;
                NostrResolvedName = identifier;
                NostrResolvedKey = key;
            });
            return ("Domain mapping ready. Review the public key before sending.", 0UL);
        });
    }

    public string SendNostrInvite(string recipient)
    {
        if (recipient.Contains('@'))
        {
            if (recipient != NostrResolvedName || NostrResolvedKey.Length != 64)
                return "Resolve this name and review its public key first.";
            recipient = NostrResolvedKey;
        }
        var key = recipient;
        return PrepareNostr((n, relays, cancellation) =>
        {
            cancellation.ThrowIfCancellationRequested();
            var job = n.NostrSendInvite(relays, key);
            return ("Invite queued; waiting for relay confirmation.", job);
        });
    }

    public string CheckNostrInbox() => PrepareNostr((n, relays, cancellation) =>
    {
        cancellation.ThrowIfCancellationRequested();
        var job = n.NostrCheckInbox(relays);
        return ("Checking chosen relays…", job);
    });

    private string Guard(Func<LanternNode, string> action)
    {
        if (node is null)
            return "Lantern is off.";
        try
        {
            return action(node);
        }
        catch (LanternException ex)
        {
            return ex.Status switch
            {
                -4 => "not found (not a friend, or already gone)",
                -6 => "that is not a valid ticket",
                -9 => "too long",
                _ => ex.Message,
            };
        }
    }

    /// <summary>Loads the last messages with a friend from SQLite, once per session per friend.</summary>
    public void EnsureHistory(FriendView f)
    {
        var c = FriendChat(f.Id);
        if (node is null || !historyLoaded.Add(f.Id))
            return;
        var lines = NativeJson.History(node.FriendHistoryJson(f.Key, 100)).Select(h => new ChatLine(
            DateTimeOffset.FromUnixTimeMilliseconds(h.SentAt), h.Outgoing ? "" : f.Display, ChatText.Clean(h.Text), h.Outgoing, h.Id,
            h.Outgoing ? (h.DeliveredAt is null ? LineState.Pending : LineState.Delivered) : LineState.None));
        c.LoadHistory(lines);
    }

    public string SendToFriend(FriendView f, string text) => Guard(n =>
    {
        text = ChatText.Clamp(text.Trim(), ChatText.MaxChatBytes);
        if (text.Length == 0)
            return "";
        ulong id = n.FriendSend(f.Key, text);
        FriendChat(f.Id).Add(new ChatLine(DateTimeOffset.Now, "", text, true, id, LineState.Pending), false);
        return "";
    });

    public string SendToChannel(ChannelInfo c, string text) => Guard(n =>
    {
        text = ChatText.Clamp(text.Trim(), ChatText.MaxChannelBytes);
        if (text.Length == 0)
            return "";
        if (config.ChannelHistoryEnabled && n.SupportsChannelHistory)
            n.RoomSendWithHistory(c.Room, text); // Own verified event supplies its signature.
        else
        {
            n.RoomSend(c.Room, text);
            ChannelChat(c).Add(new ChatLine(DateTimeOffset.Now, "", text, true), false);
        }
        return "";
    });

    public string StartPull(int seconds, string channelName)
    {
        if (!config.PullTimersEnabled) return "Channel pull timers are disabled in Settings.";
        if (seconds is < 1 or > 60) return "Pull duration must be 1–60 seconds.";
        var key = PullChannels.Resolve(Channels.Select(c => (c.Key, c.Label)), channelName, OpenConversation);
        var channel = Channels.FirstOrDefault(c => c.Key == key);
        if (channel == null) return "Choose one joined channel by name, or open its conversation first.";
        if (config.Muted.Contains(channel.Key)) return "Unmute the channel before starting a pull timer.";
        if (Pull.Remaining(Environment.TickCount64) is > 0) return "A Lantern pull timer is already running.";
        var error = SendToChannel(channel, PullEnvelope.Encode(seconds));
        if (error.Length == 0) Pull.Start(seconds, channel.Key, channel.Label, Environment.TickCount64);
        return error;
    }

    public (string Ticket, string Error) CreateInvite(uint ttlSeconds)
    {
        string ticket = "";
        var err = Guard(n =>
        {
            ticket = n.InviteCreate(ttlSeconds);
            return "";
        });
        return (ticket, err);
    }

    public string AcceptInvite(string pasted) => Guard(n =>
    {
        var (kind, ticket) = Tickets.Find(pasted);
        if (kind != TicketKind.FriendInvite)
            return "That is " + Tickets.Describe(kind) + ", not a friend invite.";
        n.InviteAccept(ticket);
        return "";
    });

    public string CreateChannel(string label, bool sealedChannel = false) => Guard(n =>
    {
        label = ChatText.Clamp(ChatText.Clean(label), 40);
        if (label.Length == 0)
            return "Give the channel a name.";
        if (sealedChannel && !n.SupportsSealedChannels) return "Update Lantern to create sealed channels.";
        ulong room = sealedChannel ? n.ChannelCreateSealed(label) : n.ChannelCreate(label);
        var saved = new SavedChannel { Label = label, Ticket = n.RoomTicket(room) };
        config.Channels.Add(saved);
        save();
        Joined(room, saved);
        return "";
    });

    public string JoinChannel(string pasted, string label = "") => Guard(n =>
    {
        var (kind, ticket) = Tickets.Find(pasted);
        if (kind is not (TicketKind.Room or TicketKind.SealedRoom))
            return "That is " + Tickets.Describe(kind) + ", not a channel ticket.";
        if (kind == TicketKind.SealedRoom && !n.SupportsSealedChannels) return "Update Lantern to join sealed channels. This ticket cannot be joined as an ordinary channel.";
        if (Channels.Any(c => c.Ticket == ticket))
            return "";
        var saved = new SavedChannel { Label = label.Length > 0 ? label : "channel", Ticket = ticket };
        if (!JoinSaved(saved))
            return "could not join";
        config.Channels.Add(saved);
        save();
        ChannelInvites.RemoveAll(c => c.Ticket == ticket);
        return "";
    });

    private bool JoinSaved(SavedChannel saved)
    {
        if (node is null)
            return false;
        try
        {
            if (saved.Ticket.StartsWith("ltsealed", StringComparison.Ordinal) && !node.SupportsSealedChannels)
            {
                Say("Update Lantern to rejoin this sealed channel.");
                return false;
            }
            ulong room = node.RoomJoin(RoomScope.Custom, saved.Label, saved.Ticket);
            Joined(room, saved);
            return true;
        }
        catch (LanternException ex)
        {
            Say($"Could not rejoin #{saved.Label}: {ex.Message}");
            return false;
        }
    }

    private void Joined(ulong room, SavedChannel saved)
    {
        var c = new ChannelInfo(room, saved.Label, saved.Ticket);
        Channels.Add(c);
        ChannelChat(c);
        if (node?.SupportsChannelHistory == true)
            node.RoomHistoryEnable(room, config.ChannelHistoryEnabled);
        // Other members learn the name the player typed, nothing else.
        if (config.DisplayName.Trim().Length > 0)
            node?.PresenceSet(room, Encoding.UTF8.GetBytes(ChatText.Clamp(config.DisplayName.Trim(), ChatText.MaxName * 4)));
    }

    public string SetChannelHistory(bool enabled)
    {
        config.ChannelHistoryEnabled = enabled;
        if (node is null) return "";
        if (!node.SupportsChannelHistory) return enabled ? "Update Lantern's native library for channel catch-up." : "";
        return Guard(n => { foreach (var c in Channels) n.RoomHistoryEnable(c.Room, enabled); return ""; });
    }

    public string RefreshChannelHistory(ChannelInfo c) => Guard(n =>
    {
        if (!config.ChannelHistoryEnabled) return "Enable channel catch-up in Settings first.";
        n.RoomHistoryEnable(c.Room, true);
        return "";
    });

    public string ClearChannelHistory(ChannelInfo c) => Guard(n =>
    {
        n.RoomHistoryClear(c.Room); // Clear the view only on native success acknowledgement.
        return "";
    });

    public string LeaveChannel(ChannelInfo c) => Guard(n =>
    {
        ForgetFileOffers(o => o.Room == c.Room);
        n.RoomLeave(c.Room);
        Channels.Remove(c);
        outgoingFiles.RemoveAll(o => o.Room == c.Room);
        config.Channels.RemoveAll(s => s.Ticket == c.Ticket);
        save();
        return "";
    });

    public void IgnoreChannelInvite(ChannelInviteOffer offer) => ChannelInvites.Remove(offer);

    private bool FileAllowed(FileOffer offer) => IsOn
        && (offer.Room == 0 ? Friends.Any(f => f.Id == offer.Peer)
            : Channel(offer.Room) is { } channel && !config.Muted.Contains(channel.Key))
        && !config.Muted.Contains(ChatBook.FriendKey(offer.Peer))
        && !Blocked.Contains(offer.Peer, StringComparer.OrdinalIgnoreCase);

    public string ClearSharedFileCache()
    {
        if (!CanClearSharedCache) return "Turn Lantern off and wait for background work to finish before clearing its shared-file cache.";
        SharedCacheBusy = true;
        SharedCacheStatus = "Clearing shared-file cache…";
        Background(() =>
        {
            string result;
            try
            {
                lock (nostrWorkGate)
                {
                    var removed = SharedFileCache.Clear(Path.GetDirectoryName(dbPath)!);
                    result = $"Shared-file cache cleared ({removed / 1024.0 / 1024:F1} MiB). Saved downloads and identity data were kept.";
                }
            }
            catch (Exception) { result = "The shared-file cache could not be completely cleared. Locked, linked or unexpected files may remain."; }
            onFramework.Enqueue(() => { SharedCacheBusy = false; SharedCacheStatus = result; });
        });
        return "";
    }

    public string OfferFile(ChannelInfo channel, string path) => PrepareFileOffer(channel.Room, channel.Key, path);
    public string OfferFriendFile(FriendView friend, string path) => PrepareFileOffer(0, ChatBook.FriendKey(friend.Id), path);

    private bool PublicationAllowed(ulong room, string key) => !config.Muted.Contains(key)
        && (room == 0 ? SupportsFriendFiles && Friends.Any(f => ChatBook.FriendKey(f.Id) == key && f.Linked
            && !Blocked.Contains(f.Id, StringComparer.OrdinalIgnoreCase)) : Channel(room)?.Key == key);

    private string PrepareFileOffer(ulong room, string key, string path)
    {
        var current = node;
        if (current == null || !SupportsFileDownloads) return "Update Lantern to share files.";
        if (FileSendBusy) return "Wait for the current file to finish preparing.";
        if (!PublicationAllowed(room, key)) return "Choose an unmuted channel or a connected friend first.";
        if (sharedFileCount >= 16 || sharedFileBytes + FileOffers.MaxBytes > 64 * 1024 * 1024)
            return "The session file-sharing limit has been reached. Turn Lantern off to reset this limit; use Clear shared-file cache to remove cached copies.";
        var sealedUpload = room != 0 && Channel(room)?.Sealed == true;
        if (sealedUpload && !current.SupportsSealedChannels) return "Update Lantern to share files in sealed channels.";
        var cacheBudget = (ulong)Math.Clamp(config.SharedCacheBudgetMiB, 64, 1024) * 1024 * 1024;
        FileSendBusy = true;
        FileSendStatus = "Preparing file…";
        var expectedGeneration = generation;
        Background(() =>
        {
            ulong handle = 0;
            ulong size = 0;
            var name = "";
            var error = "";
            try
            {
                SharedFileCache.CheckBudget(Path.GetDirectoryName(dbPath)!, cacheBudget);
                var snapshot = FileDownloads.ReadOffer(path);
                size = (ulong)snapshot.Bytes.Length;
                name = snapshot.Name;
                lock (nostrWorkGate)
                {
                    if (expectedGeneration != Volatile.Read(ref generation)) return;
                    handle = sealedUpload ? current.BlobAddSealedBytes(room, snapshot.Bytes, snapshot.Name)
                        : current.BlobAddBytes(snapshot.Bytes, snapshot.Name);
                }
            }
            catch (SharedCacheException ex) { error = ex.Message; }
            catch (Exception) { error = "The file could not be prepared. Choose a readable file of at most 16 MiB."; }
            onFramework.Enqueue(() =>
            {
                if (expectedGeneration != generation || node != current) return;
                if (error.Length > 0)
                {
                    FileSendBusy = false;
                    FileSendStatus = error;
                    earlyFilePublication.Clear();
                    return;
                }
                sharedFileBytes += size;
                sharedFileCount++;
                filePublication = new OutgoingFileOffer(handle, room, key, name, size);
                FileSendStatus = "Preparing file…";
                if (earlyFilePublication.Remove(handle, out var success)) FilePublicationResult(handle, success);
                earlyFilePublication.Clear();
            });
        });
        return "";
    }

    private void FilePublicationResult(ulong handle, bool success)
    {
        if (!FileSendBusy) return;
        if (filePublication is not { } publication)
        {
            if (earlyFilePublication.Count < 8) earlyFilePublication.TryAdd(handle, success);
            return;
        }
        if (publication.Handle != handle) return;
        filePublication = null;
        FileSendBusy = false;
        if (!success) { FileSendStatus = "The file could not be prepared."; return; }
        if (!PublicationAllowed(publication.Room, publication.Key))
        {
            FileSendStatus = "The conversation is no longer available; the file was not offered.";
            return;
        }
        FileSendStatus = Guard(n => {
            if (publication.Room == 0) n.FriendFileOffer(Convert.FromHexString(publication.Key[2..]), handle);
            else n.BlobOffer(publication.Room, handle);
            return "";
        });
        if (FileSendStatus.Length == 0)
        {
            outgoingFiles.Add(publication);
            FileSendStatus = "File offer queued. Delivery is not confirmed; recipients choose whether to download it.";
        }
    }

    public string WithdrawFileOffer(OutgoingFileOffer offer) => Guard(n =>
    {
        if (!outgoingFiles.Contains(offer)) return "That outgoing offer is no longer available.";
        // Withdrawal remains possible when muted; it never starts a new publication.
        if (offer.Room == 0)
            n.FriendFileWithdraw(Convert.FromHexString(offer.Key[2..]), offer.Handle);
        else
            n.BlobUnoffer(offer.Room, offer.Handle);
        outgoingFiles.Remove(offer);
        return "Withdrawal queued. Copies already downloaded remain with recipients; cached local bytes are unchanged.";
    });

    public string DownloadFile(ulong handle)
    {
        var current = node;
        var offer = FileOffers.Find(handle);
        if (current == null || !SupportsFileDownloads) return "Update Lantern to download shared files.";
        if (offer == null || !FileAllowed(offer)) return "That file offer is no longer available.";
        var directory = Path.Combine(Path.GetDirectoryName(dbPath)!, "downloads");
        var destination = FileOffers.BeginFetch(handle, directory, FileAllowed);
        if (destination == null) return "Two downloads can run at once; wait or cancel one first.";
        var expectedGeneration = generation;
        var downloadBudget = (ulong)Math.Clamp(config.DownloadBudgetMiB, 32, 1024) * 1024 * 1024;
        var retentionDays = Math.Clamp(config.DownloadRetentionDays, 0, 90);
        var protectedPaths = FileOffers.All.Where(o => o.State is FileOfferState.Fetching or FileOfferState.Canceling)
            .Select(o => o.Destination).Where(p => p.Length > 0).ToArray();
        Background(() =>
        {
            var error = "";
            IReadOnlyList<string> deleted = [];
            try
            {
                lock (fileStorageGate)
                {
                    if (retentionDays > 0) deleted = FileDownloads.CleanupExpired(Path.GetDirectoryName(dbPath)!, retentionDays, protectedPaths, DateTime.UtcNow);
                    FileDownloads.PrepareDirectory(Path.GetDirectoryName(dbPath)!, downloadBudget);
                }
            }
            catch (DownloadBudgetException ex) { error = ex.Message; }
            catch (Exception) { error = "The download folder could not be prepared safely."; }
            onFramework.Enqueue(() =>
            {
                if (generation != expectedGeneration || node != current) return;
                FileOffers.RemoveWhere(o => o.State == FileOfferState.Saved && deleted.Contains(o.Destination));
                var pending = FileOffers.Find(handle);
                if (pending?.Destination != destination) return; // A canceled preparation may have been retried.
                if (pending?.State != FileOfferState.Fetching || !FileAllowed(pending))
                {
                    FileOffers.Complete(handle, false);
                    return;
                }
                if (error.Length == 0)
                {
                    try { current.BlobFetchBounded(handle, destination); queuedFileDownloads.Add(handle); }
                    catch (Exception) { error = "The file download could not start."; }
                }
                if (error.Length > 0) { FileOffers.Complete(handle, false); FileDownloadStatus = error; }
            });
        });
        FileDownloadStatus = "";
        return "";
    }

    public string CancelFileDownload(ulong handle)
    {
        var offer = FileOffers.Find(handle);
        if (offer?.State != FileOfferState.Fetching) return "That download is not running.";
        if (!queuedFileDownloads.Contains(handle))
        {
            FileOffers.CancelRequested(handle);
            FileOffers.Complete(handle, false);
            return "";
        }
        try { node?.BlobCancelBounded(handle); }
        catch (LanternException)
        {
            return "The download has finished; waiting for its result.";
        }
        FileOffers.CancelRequested(handle);
        return "";
    }

    private void ForgetFileOffers(Func<FileOffer, bool> remove)
    {
        foreach (var offer in FileOffers.All.Where(remove).ToList())
            if (offer.State is FileOfferState.Fetching or FileOfferState.Canceling && SupportsFileDownloads)
                try { node?.BlobCancelBounded(offer.Handle); } catch (LanternException) { }
        FileOffers.RemoveWhere(remove);
    }

    public string ResealChannel(ChannelInfo channel, IReadOnlyCollection<string> selectedFriends) => Guard(n =>
    {
        if (!n.SupportsSealedChannels) return "Update Lantern to reseal channels.";
        if (!channel.Sealed || !Channels.Contains(channel)) return "Choose a current sealed channel.";
        var recipients = selectedFriends.Distinct(StringComparer.OrdinalIgnoreCase)
            .Select(id => Friends.FirstOrDefault(f => string.Equals(f.Id, id, StringComparison.OrdinalIgnoreCase))).ToList();
        if (recipients.Any(f => f == null || !f.Linked || Blocked.Contains(f.Id, StringComparer.OrdinalIgnoreCase)))
            return "A selected friend is no longer connected. Review the selection and try again.";
        ulong room = n.ChannelReseal(channel.Room);
        var saved = new SavedChannel { Label = channel.Label, Ticket = n.RoomTicket(room) };
        config.Channels.Add(saved);
        if (config.Muted.Contains(channel.Key)) config.Muted.Add(ChatBook.ChannelKey(saved.Ticket));
        save();
        Joined(room, saved);
        int failed = 0;
        foreach (var friend in recipients)
            try { n.ChannelInvite(friend!.Key, room); } catch (LanternException) { failed++; }
        return failed == 0 ? "" : $"New sealed channel created; {failed} invitation(s) could not be queued. Retry from the friends menu.";
    });

    public string InviteToChannel(FriendView f, ChannelInfo c) => Guard(n =>
    {
        if (!Channels.Contains(c)) return "That channel is no longer joined.";
        var recipient = Friends.FirstOrDefault(current => string.Equals(current.Id, f.Id, StringComparison.OrdinalIgnoreCase));
        if (recipient is null || !recipient.Linked || Blocked.Contains(recipient.Id, StringComparer.OrdinalIgnoreCase))
            return "That friend is no longer available for an invitation.";
        if (c.Sealed && !n.SupportsSealedChannels) return "Update Lantern to invite friends to sealed channels.";
        n.ChannelInvite(recipient.Key, c.Room);
        ChannelChat(c).Add(System("Invitation queued for " + recipient.Display + "."), false);
        return "";
    });

    public string SetName(string name) => Guard(n =>
    {
        config.DisplayName = ChatText.Clamp(ChatText.Clean(name), ChatText.MaxName * 4);
        save();
        n.SetDisplayName(config.DisplayName);
        foreach (var c in Channels)
            n.PresenceSet(c.Room, Encoding.UTF8.GetBytes(config.DisplayName));
        return "";
    });

    public string SetPresence(uint presence, string note) => Guard(n =>
    {
        config.Presence = presence;
        config.PresenceNote = ChatText.Clamp(ChatText.Clean(note), ChatText.MaxNote * 4);
        save();
        n.SetPresence((PresenceStatus)presence, config.PresenceNote);
        idleAway.ManualChanged();
        return "";
    });

    public bool SupportsActivityTags => node?.SupportsActivityTags ?? false;

    public string SetActivityTags(IEnumerable<string> tags)
    {
        var cleaned = ActivityTags.Normalize(tags);
        if (IsOn)
        {
            var error = Guard(n => { n.SetActivityTags(cleaned); return ""; });
            if (error.Length > 0) return error;
        }
        config.ActivityTags = cleaned;
        save();
        return "";
    }

    public string RemoveFriend(FriendView f) => Guard(n =>
    {
        n.FriendRemove(f.Key);
        outgoingFiles.RemoveAll(o => o.Room == 0 && o.Key == ChatBook.FriendKey(f.Id));
        RefreshFriends();
        return "";
    });

    public string Block(string id) => Guard(n =>
    {
        n.Block(Convert.FromHexString(id));
        outgoingFiles.RemoveAll(o => o.Room == 0 && string.Equals(o.Key, ChatBook.FriendKey(id), StringComparison.OrdinalIgnoreCase));
        RefreshFriends();
        RefreshBlocked();
        ForgetFileOffers(o => string.Equals(o.Peer, id, StringComparison.OrdinalIgnoreCase));
        return "";
    });

    public string Unblock(string id) => Guard(n =>
    {
        n.Unblock(Convert.FromHexString(id));
        RefreshBlocked();
        return "";
    });

    public void ToggleMute(string key)
    {
        if (!config.Muted.Remove(key))
        {
            config.Muted.Add(key);
            ForgetFileOffers(o => ChatBook.FriendKey(o.Peer) == key || Channel(o.Room)?.Key == key);
        }
        save();
    }

    public string JoinAuthor()
    {
        if (!AuthorIdentity.Configured)
            return "This build has no author channel configured yet.";
        return Guard(n =>
        {
            var nodes = AuthorIdentity.Nodes;
            n.SetSupportContacts(nodes);
            AuthorRoom = n.AuthorJoin(AuthorIdentity.Key!, nodes);
            Announcements = NativeJson.Announcements(n.AnnouncementsJson(AuthorIdentity.Key!, 20)).ToList();
            var chat = AuthorChat();
            if (chat.Lines.Count == 0)
            {
                foreach (var a in Announcements.AsEnumerable().Reverse())
                    chat.Add(new ChatLine(DateTimeOffset.FromUnixTimeSeconds(a.IssuedAt), "Author", a.Title + (a.Body.Length > 0 ? "\n" + a.Body : ""), false, a.Seq, Verified: true), false);
            }

            return "";
        });
    }

    public string LeaveAuthor() => Guard(n =>
    {
        if (AuthorRoom is { } room)
            n.RoomLeave(room);
        AuthorRoom = null;
        n.SetSupportContacts([]);
        return "";
    });

    public string WriteToAuthor(string text) => Guard(n =>
    {
        if (AuthorRoom is null || AuthorIdentity.Nodes.Count == 0)
            return "Join the author channel first.";
        text = ChatText.Clamp(text.Trim(), ChatText.MaxChatBytes);
        if (text.Length == 0)
            return "";
        ulong id = n.SupportSend(AuthorIdentity.Nodes[0], text);
        SupportChat().Add(new ChatLine(DateTimeOffset.Now, "", text, true, id, LineState.Pending), false);
        return "";
    });

    /// <summary>
    /// Test reachability. With Lantern on, the running node is tested. With it off, a throwaway in-memory node is
    /// opened for the test only (the player asked for it by name) and closed straight after.
    /// </summary>
    public void SelfTest(Action<SelfTestReport?, string> done)
    {
        if (node is not null)
        {
            try
            {
                selftests[node.SelfTest()] = done;
            }
            catch (LanternException ex)
            {
                done(null, ex.Message);
            }

            return;
        }

        var options = Options(null);
        Background(() =>
        {
            SelfTestReport? report = null;
            string raw = "timed out";
            try
            {
                using var temp = LanternNode.Open(options);
                ulong h = temp.SelfTest();
                var until = DateTime.UtcNow + TimeSpan.FromSeconds(60);
                while (DateTime.UtcNow < until && report is null)
                {
                    foreach (var e in temp.Poll())
                    {
                        if (e.Kind == LanternEventKind.SelfTest && e.Handle == h)
                        {
                            raw = e.Text;
                            report = NativeJson.SelfTest(raw);
                        }
                    }

                    Thread.Sleep(100);
                }
            }
            catch (Exception ex)
            {
                raw = ex.Message;
            }

            onFramework.Enqueue(() => done(report, raw));
        });
    }
}
