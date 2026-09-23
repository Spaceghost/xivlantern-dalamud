using System.Collections.Concurrent;
using System.Text;
using Dalamud.Plugin.Services;
using Linkpearl;
using XivLinkpearl.Core;

namespace XivLinkpearl.Plugin;

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
    public string Key => ChatBook.ChannelKey(Ticket);
    public HashSet<string> Members { get; } = [];
}

/// <summary>A channel a friend invited us to. Nothing is joined until the player says so.</summary>
public sealed record ChannelInviteOffer(string FromId, string FromName, string Ticket, string Label);

/// <summary>
/// Owns the Linkpearl node. Opening and closing happen on a worker thread (both block); everything else — commands,
/// <c>lp_poll</c>, the chat model — happens on the framework thread, which is also the thread that draws, so the
/// model needs no locks. Nothing is loaded or connected until <see cref="Start"/>, which only runs once the player
/// has turned Linkpearl on.
/// </summary>
public sealed class LinkpearlService : IDisposable
{
    public const string AppId = "ffxiv-linkpearl/1";
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
    private LinkpearlNode? node;
    /// <summary>Opens, closes and throwaway selftests still running. Unload waits for them: no Rust thread may
    /// outlive the plugin's code.</summary>
    private readonly List<Task> background = [];
    private DateTime nextFriendRefresh;
    private int generation;

    public LinkpearlService(string nativeDir, string configDir, Configuration config, Action save, IPluginLog log, IFramework framework)
    {
        this.nativeDir = nativeDir;
        dbPath = Path.Combine(configDir, "linkpearl.sqlite");
        this.config = config;
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
    public IReadOnlyList<FriendView> Friends { get; private set; } = [];
    public List<ChannelInfo> Channels { get; } = [];
    public List<ChannelInviteOffer> ChannelInvites { get; } = [];
    public List<Announcement> Announcements { get; private set; } = [];
    public IReadOnlyList<string> Blocked { get; private set; } = [];
    public ulong? AuthorRoom { get; private set; }
    public string DataPath => dbPath;
    public bool IsOn => State == ServiceState.On && node is not null;

    /// <summary>The conversation the window is showing, so it is not toasted or counted as unread.</summary>
    public string? OpenConversation { get; set; }

    /// <summary>Raised on the framework thread: something worth a toast (or an IPC event) arrived.</summary>
    public event Action<Incoming, string, string, string>? Arrived;

    /// <summary>A line for the player: shown in the window's status area and, for commands, printed to chat.</summary>
    public event Action<string>? Notice;

    public LinkpearlStats? Stats => node?.Stats();

    public NotifySettings NotifySettings => new(IsOn, config.Toasts, config.ChannelToasts, config.FriendOnlineToasts, config.Muted);

    // ------------------------------------------------------------ lifecycle

    public void Start()
    {
        if (State is ServiceState.Starting or ServiceState.On)
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
                var opened = LinkpearlNode.Open(options);
                if (gen != Volatile.Read(ref generation))
                {
                    // Turned off, or unloading, while it was opening.
                    opened.Dispose();
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

    private LinkpearlOptions Options(string? db) => new()
    {
        NativeDirectory = nativeDir,
        DatabasePath = db,
        AppId = AppId,
        Relay = (RelayMode)(uint)config.Relay,
        RelayUrl = config.Relay == RelayChoice.Custom ? config.CustomRelayUrl.Trim() : null,
        AcceptInbound = true,
    };

    private void Opened(LinkpearlNode opened, int gen)
    {
        if (gen != generation || State != ServiceState.Starting)
        {
            // Turned off while it was opening.
            Background(opened.Dispose);
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
            if (config.RejoinChannels)
            {
                foreach (var saved in config.Channels.ToList())
                    JoinSaved(saved);
            }

            if (config.JoinAuthorChannel)
                JoinAuthor();
            RefreshFriends();
            RefreshBlocked();
            Say("Linkpearl is on. Node id " + NodeId[..16] + "…");
        }
        catch (LinkpearlException ex)
        {
            Say("Linkpearl started, but: " + ex.Message);
        }
    }

    public void Stop()
    {
        generation++;
        var n = node;
        node = null;
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
                n.Dispose();
            }
            catch (Exception ex)
            {
                log.Warning(ex, "Linkpearl: closing the node failed");
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
        Say("Linkpearl could not start: " + message);
    }

    private void Say(string line)
    {
        log.Information("Linkpearl: {Line}", line);
        Notice?.Invoke(line);
    }

    public void Dispose()
    {
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
            n?.Dispose();
        }
        catch (Exception ex)
        {
            log.Warning(ex, "Linkpearl: closing the node on unload failed");
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
                log.Error(ex, "Linkpearl: a queued action failed");
            }
        }

        if (node is null)
            return;
        try
        {
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
            log.Error(ex, "Linkpearl: handling events failed");
        }
    }

    private static string Hex(byte[] id) => Convert.ToHexStringLower(id);

    private string NameOf(string id)
    {
        var f = Friends.FirstOrDefault(x => x.Id == id);
        if (f is not null && f.Name.Length > 0)
            return f.Name;
        return memberNames.TryGetValue(id, out var n) && n.Length > 0 ? n : id[..10];
    }

    private void Handle(LinkpearlEvent e)
    {
        var peer = Hex(e.Peer);
        switch (e.Kind)
        {
            case LinkpearlEventKind.Ready:
                NodeId = e.Text;
                break;
            case LinkpearlEventKind.FriendAdded:
                RefreshFriends();
                FriendChat(peer).Add(System("You are now friends with " + ChatText.Clean(e.Text) + "."), false);
                Arrived?.Invoke(Incoming.InviteAccepted, ChatBook.FriendKey(peer), ChatText.Clean(e.Text), "is now your friend");
                break;
            case LinkpearlEventKind.FriendRemoved:
                RefreshFriends();
                break;
            case LinkpearlEventKind.InviteFailed:
                Say("That invite did not work: " + e.Text);
                break;
            case LinkpearlEventKind.FriendOnline:
                bool wasOnline = Friends.FirstOrDefault(f => f.Id == peer)?.Online ?? false;
                RefreshFriends();
                if (!wasOnline)
                    Arrived?.Invoke(Incoming.FriendOnline, ChatBook.FriendKey(peer), NameOf(peer), "is online");
                break;
            case LinkpearlEventKind.FriendOffline:
                RefreshFriends();
                break;
            case LinkpearlEventKind.FriendText:
            {
                var key = ChatBook.FriendKey(peer);
                var text = ChatText.Clean(e.Text);
                FriendChat(peer).Add(new ChatLine(DateTimeOffset.Now, NameOf(peer), text, false, e.Handle), NotifyPolicy.CountsUnread(key, NotifySettings, OpenConversation));
                Arrived?.Invoke(Incoming.FriendText, key, NameOf(peer), text);
                break;
            }
            case LinkpearlEventKind.FriendDelivered:
                FriendChat(peer).Mark(e.Handle, LineState.Delivered);
                break;
            case LinkpearlEventKind.ChannelInvite:
            {
                var (kind, ticket) = Tickets.Find(e.Text);
                if (kind == TicketKind.Room && !ChannelInvites.Any(c => c.Ticket == ticket) && !Channels.Any(c => c.Ticket == ticket))
                {
                    ChannelInvites.Add(new ChannelInviteOffer(peer, NameOf(peer), ticket, "a channel"));
                    Arrived?.Invoke(Incoming.FriendText, ChatBook.FriendKey(peer), NameOf(peer), "invited you to a channel (open Linkpearl to join or ignore)");
                }

                break;
            }
            case LinkpearlEventKind.RoomJoined when Channel(e.Handle) is { } c:
                ChannelChat(c).Add(System("Joined #" + c.Label + "."), false);
                break;
            case LinkpearlEventKind.PeerJoined when Channel(e.Handle) is { } c:
                c.Members.Add(peer);
                break;
            case LinkpearlEventKind.PeerLeft when Channel(e.Handle) is { } c:
                c.Members.Remove(peer);
                break;
            case LinkpearlEventKind.Presence when Channel(e.Handle) is not null:
                // In a channel, room presence is the name a member typed.
                memberNames[peer] = ChatText.Clamp(ChatText.Clean(e.Text), ChatText.MaxName);
                break;
            case LinkpearlEventKind.Message when Channel(e.Handle) is { } c:
            {
                var text = ChatText.Clean(Encoding.UTF8.GetString(e.Data));
                if (text.Length == 0)
                    break;
                ChannelChat(c).Add(new ChatLine(DateTimeOffset.Now, NameOf(peer), ChatText.Clamp(text, ChatText.MaxChannelBytes), false),
                    NotifyPolicy.CountsUnread(c.Key, NotifySettings, OpenConversation));
                Arrived?.Invoke(Incoming.ChannelLine, c.Key, NameOf(peer) + " in #" + c.Label, text);
                break;
            }
            case LinkpearlEventKind.RateLimited:
                Say($"{NameOf(peer)} is sending too fast; some of their {e.Text} messages were dropped.");
                break;
            case LinkpearlEventKind.Announcement:
                if (NativeJson.AnnouncementEvent(e.Text) is { } a)
                {
                    Announcements.RemoveAll(x => x.Seq == a.Seq);
                    Announcements.Insert(0, a);
                    AuthorChat().Add(new ChatLine(DateTimeOffset.FromUnixTimeSeconds(a.IssuedAt), "Author", a.Title + (a.Body.Length > 0 ? "\n" + a.Body : ""), false, a.Seq, Verified: true),
                        NotifyPolicy.CountsUnread(ChatBook.AuthorKey, NotifySettings, OpenConversation));
                    Arrived?.Invoke(Incoming.Announcement, ChatBook.AuthorKey, "Author (verified)", a.Title);
                }

                break;
            case LinkpearlEventKind.SupportReply:
            {
                var text = ChatText.Clean(e.Text);
                SupportChat().Add(new ChatLine(DateTimeOffset.Now, "Author", text, false, e.Handle, Verified: true),
                    NotifyPolicy.CountsUnread(ChatBook.SupportKey, NotifySettings, OpenConversation));
                Arrived?.Invoke(Incoming.SupportReply, ChatBook.SupportKey, "Author", text);
                break;
            }
            case LinkpearlEventKind.SupportDelivered:
                SupportChat().Mark(e.Handle, LineState.Delivered);
                break;
            case LinkpearlEventKind.SupportFailed:
                SupportChat().Mark(e.Handle, LineState.Failed);
                SupportChat().Add(System("Not delivered: " + e.Text), false);
                break;
            case LinkpearlEventKind.SelfTest:
                if (selftests.Remove(e.Handle, out var done))
                    done(NativeJson.SelfTest(e.Text), e.Text);
                break;
            case LinkpearlEventKind.Error:
                log.Warning("Linkpearl: {Message}", e.Text);
                break;
            case LinkpearlEventKind.Log:
                log.Debug("Linkpearl: {Message}", e.Text);
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

    private string Guard(Func<LinkpearlNode, string> action)
    {
        if (node is null)
            return "Linkpearl is off.";
        try
        {
            return action(node);
        }
        catch (LinkpearlException ex)
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
        n.RoomSend(c.Room, text);
        ChannelChat(c).Add(new ChatLine(DateTimeOffset.Now, "", text, true), false);
        return "";
    });

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

    public string CreateChannel(string label) => Guard(n =>
    {
        label = ChatText.Clamp(ChatText.Clean(label), 40);
        if (label.Length == 0)
            return "Give the channel a name.";
        ulong room = n.ChannelCreate(label);
        var saved = new SavedChannel { Label = label, Ticket = n.RoomTicket(room) };
        config.Channels.Add(saved);
        save();
        Joined(room, saved);
        return "";
    });

    public string JoinChannel(string pasted, string label = "") => Guard(n =>
    {
        var (kind, ticket) = Tickets.Find(pasted);
        if (kind != TicketKind.Room)
            return "That is " + Tickets.Describe(kind) + ", not a channel ticket.";
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
            ulong room = node.RoomJoin(RoomScope.Custom, saved.Label, saved.Ticket);
            Joined(room, saved);
            return true;
        }
        catch (LinkpearlException ex)
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
        // Other members learn the name the player typed, nothing else.
        if (config.DisplayName.Trim().Length > 0)
            node?.PresenceSet(room, Encoding.UTF8.GetBytes(ChatText.Clamp(config.DisplayName.Trim(), ChatText.MaxName * 4)));
    }

    public string LeaveChannel(ChannelInfo c) => Guard(n =>
    {
        n.RoomLeave(c.Room);
        Channels.Remove(c);
        config.Channels.RemoveAll(s => s.Ticket == c.Ticket);
        save();
        return "";
    });

    public void IgnoreChannelInvite(ChannelInviteOffer offer) => ChannelInvites.Remove(offer);

    public string InviteToChannel(FriendView f, ChannelInfo c) => Guard(n =>
    {
        n.ChannelInvite(f.Key, c.Room);
        ChannelChat(c).Add(System("Invited " + f.Display + "."), false);
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
        return "";
    });

    public string RemoveFriend(FriendView f) => Guard(n =>
    {
        n.FriendRemove(f.Key);
        RefreshFriends();
        return "";
    });

    public string Block(string id) => Guard(n =>
    {
        n.Block(Convert.FromHexString(id));
        RefreshFriends();
        RefreshBlocked();
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
            config.Muted.Add(key);
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
    /// Test reachability. With Linkpearl on, the running node is tested. With it off, a throwaway in-memory node is
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
            catch (LinkpearlException ex)
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
                using var temp = LinkpearlNode.Open(options);
                ulong h = temp.SelfTest();
                var until = DateTime.UtcNow + TimeSpan.FromSeconds(60);
                while (DateTime.UtcNow < until && report is null)
                {
                    foreach (var e in temp.Poll())
                    {
                        if (e.Kind == LinkpearlEventKind.SelfTest && e.Handle == h)
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
