using System.Numerics;
using Dalamud.Bindings.ImGui;
using Dalamud.Interface.Windowing;
using XivLinkpearl.Core;

namespace XivLinkpearl.Plugin;

/// <summary>
/// The Linkpearl window. Off: the privacy note, the relay choice and the switch. On: friends and channels on the
/// left, the open conversation on the right; an Add tab for invites and channels; the author's channel; settings.
/// Nothing here accepts anything on its own: every invite, ticket and channel needs the player's click.
/// </summary>
public sealed class MainWindow : Window
{
    private static readonly Vector4 Accent = new(0.96f, 0.78f, 0.36f, 1f);
    private static readonly Vector4 Good = new(0.45f, 0.86f, 0.58f, 1f);
    private static readonly Vector4 Away = new(0.95f, 0.80f, 0.35f, 1f);
    private static readonly Vector4 Busy = new(1.00f, 0.45f, 0.40f, 1f);
    private static readonly Vector4 Dim = new(0.60f, 0.60f, 0.64f, 1f);
    private static readonly string[] Statuses = ["invisible", "online", "away", "busy"];
    private static readonly string[] RelayNames = ["n0 relays (default)", "Relays off (LAN / direct only)", "My own relay"];

    private readonly LinkpearlService service;
    private readonly Configuration config;
    private readonly Action save;
    private string selected = "";
    private string draft = "";
    private string nameDraft = "";
    private string noteDraft = "";
    private string invite = "";
    private string paste = "";
    private string channelName = "";
    private string supportDraft = "";
    private string status = "";
    private string[] selftest = [];
    private int scrolledVersion = -1;
    private bool focusInput;
    private bool showSettings;

    public MainWindow(LinkpearlService service, Configuration config, Action save)
        : base("Linkpearl###XivLinkpearl")
    {
        this.service = service;
        this.config = config;
        this.save = save;
        nameDraft = config.DisplayName;
        noteDraft = config.PresenceNote;
        Size = new Vector2(760, 520);
        SizeCondition = ImGuiCond.FirstUseEver;
        SizeConstraints = new WindowSizeConstraints { MinimumSize = new Vector2(480, 320), MaximumSize = new Vector2(float.MaxValue, float.MaxValue) };
        service.Notice += line => status = line;
    }

    public void ShowSettings()
    {
        IsOpen = true;
        showSettings = true;
    }

    /// <summary>Open on a friend's chat (by name or id prefix), or just open.</summary>
    public void OpenChat(string who)
    {
        IsOpen = true;
        if (who.Length > 0 && FriendLookup.Find(service.Friends, who).Friend is { } f)
            Select(ChatBook.FriendKey(f.Id));
    }

    public void ShowSelfTest(IEnumerable<string> lines) => selftest = lines.ToArray();

    public override void OnClose() => service.OpenConversation = null;

    public override void Draw()
    {
        if (!config.Enabled || service.State is ServiceState.Off or ServiceState.Failed && !service.IsOn)
        {
            DrawWelcome();
            return;
        }

        if (!ImGui.BeginTabBar("##linkpearl-tabs"))
            return;
        if (ImGui.BeginTabItem("Chats"))
        {
            DrawChats();
            ImGui.EndTabItem();
        }

        if (ImGui.BeginTabItem("Add"))
        {
            service.OpenConversation = null;
            DrawAdd();
            ImGui.EndTabItem();
        }

        if (ImGui.BeginTabItem("Author"))
        {
            DrawAuthor();
            ImGui.EndTabItem();
        }

        var flags = showSettings ? ImGuiTabItemFlags.SetSelected : ImGuiTabItemFlags.None;
        showSettings = false;
        if (ImGui.BeginTabItem("Settings", flags))
        {
            service.OpenConversation = null;
            DrawSettings();
            ImGui.EndTabItem();
        }

        ImGui.EndTabBar();
    }

    // ------------------------------------------------------------ welcome

    private void DrawWelcome()
    {
        ImGui.TextColored(Accent, "Linkpearl is off. Nothing is connected.");
        if (service.State == ServiceState.Failed)
            ImGui.TextColored(Busy, "Last start failed: " + service.LastError);
        ImGui.Spacing();
        DrawPrivacy();
        ImGui.Separator();
        DrawRelayChoice();
        ImGui.SetNextItemWidth(260);
        if (ImGui.InputTextWithHint("Name your friends see", "anything; not read from the game", ref nameDraft, 64))
        {
            config.DisplayName = ChatText.Clean(nameDraft);
            save();
        }

        bool ack = config.PrivacyAcknowledged;
        if (ImGui.Checkbox("I have read the above", ref ack))
        {
            config.PrivacyAcknowledged = ack;
            save();
        }

        ImGui.BeginDisabled(!config.PrivacyAcknowledged);
        if (ImGui.Button("Turn Linkpearl on"))
        {
            config.Enabled = true;
            save();
            service.Start();
        }

        ImGui.EndDisabled();
        ImGui.SameLine();
        DrawSelfTestButton("Test the connection (temporary node)");
        DrawSelfTestResult();
    }

    private void DrawPrivacy()
    {
        ImGui.PushTextWrapPos(0);
        ImGui.TextWrapped(Privacy.Summary);
        ImGui.TextWrapped(Privacy.WhoSeesWhat);
        ImGui.TextWrapped(Privacy.Relay(config.Relay, config.CustomRelayUrl));
        ImGui.TextColored(Dim, Privacy.Storage);
        ImGui.TextColored(Dim, Privacy.TermsOfService);
        ImGui.PopTextWrapPos();
    }

    private void DrawRelayChoice()
    {
        int relay = (int)config.Relay;
        ImGui.SetNextItemWidth(260);
        if (ImGui.Combo("Relays", ref relay, RelayNames, RelayNames.Length))
        {
            config.Relay = (RelayChoice)relay;
            save();
        }

        if (config.Relay == RelayChoice.Custom)
        {
            string url = config.CustomRelayUrl;
            ImGui.SetNextItemWidth(360);
            if (ImGui.InputTextWithHint("Relay URL", "https://relay.example.org", ref url, 200))
            {
                config.CustomRelayUrl = url.Trim();
                save();
            }
        }
    }

    private void DrawSelfTestButton(string label)
    {
        if (ImGui.Button(label))
        {
            selftest = ["testing… (up to a minute)"];
            service.SelfTest((report, raw) => selftest = report is null ? ["selftest failed: " + raw] : report.Lines().ToArray());
        }

        if (ImGui.IsItemHovered())
            ImGui.SetTooltip("Checks that this game can open a connection directly and through the relay. With Linkpearl off it starts a throwaway node for the test and closes it.");
    }

    private void DrawSelfTestResult()
    {
        foreach (var line in selftest)
            ImGui.TextColored(line.Contains("FAILED", StringComparison.Ordinal) ? Busy : Dim, line);
    }

    // -------------------------------------------------------------- chats

    private void Select(string key)
    {
        selected = key;
        focusInput = true;
        scrolledVersion = -1;
    }

    private void DrawChats()
    {
        DrawMe();
        float listWidth = MathF.Min(230, ImGui.GetContentRegionAvail().X * 0.35f);
        if (ImGui.BeginChild("##lp-list", new Vector2(listWidth, -ImGui.GetFrameHeightWithSpacing()), true))
            DrawList();
        ImGui.EndChild();
        ImGui.SameLine();
        ImGui.BeginGroup();
        DrawConversation();
        ImGui.EndGroup();
        DrawStatusLine();
    }

    private void DrawMe()
    {
        int presence = (int)Math.Min(config.Presence, 3);
        ImGui.SetNextItemWidth(110);
        if (ImGui.Combo("##presence", ref presence, Statuses, Statuses.Length))
            status = service.SetPresence((uint)presence, noteDraft);
        ImGui.SameLine();
        ImGui.SetNextItemWidth(260);
        if (ImGui.InputTextWithHint("##note", "a note for friends (optional)", ref noteDraft, 120, ImGuiInputTextFlags.EnterReturnsTrue))
            status = service.SetPresence(config.Presence, noteDraft);
        ImGui.SameLine();
        ImGui.TextColored(Dim, config.DisplayName.Length > 0 ? "as " + ChatText.Label(config.DisplayName) : "(no name set)");
    }

    private static Vector4 Dot(FriendView f) => !f.Online ? Dim : f.Status switch
    {
        "away" => Away,
        "busy" => Busy,
        _ => Good,
    };

    private void DrawList()
    {
        ImGui.TextColored(Accent, "Friends");
        if (service.Friends.Count == 0)
            ImGui.TextColored(Dim, "none yet — see Add");
        foreach (var f in service.Friends)
        {
            var key = ChatBook.FriendKey(f.Id);
            var chat = service.Book.Find(key);
            int unread = chat?.Unread ?? 0;
            ImGui.TextColored(Dot(f), "●");
            ImGui.SameLine();
            var label = ChatText.Label(f.Display) + (unread > 0 ? $" ({unread})" : "") + (config.Muted.Contains(key) ? " · muted" : "") + "##" + f.Id;
            if (ImGui.Selectable(label, selected == key))
            {
                Select(key);
                service.EnsureHistory(f);
            }

            if (ImGui.IsItemHovered())
                ImGui.SetTooltip($"{f.StatusLabel}{(f.Online && f.Note.Length > 0 ? " — " + f.Note : "")}\n{f.Id}");
            FriendMenu(f, key);
        }

        ImGui.Spacing();
        ImGui.TextColored(Accent, "Channels");
        if (service.Channels.Count == 0)
            ImGui.TextColored(Dim, "none — see Add");
        foreach (var c in service.Channels.ToList())
        {
            var chat = service.ChannelChat(c);
            var label = "#" + ChatText.Label(c.Label) + (chat.Unread > 0 ? $" ({chat.Unread})" : "") + (config.Muted.Contains(c.Key) ? " · muted" : "") + "##" + c.Room;
            if (ImGui.Selectable(label, selected == c.Key))
                Select(c.Key);
            if (ImGui.BeginPopupContextItem("##ch" + c.Room))
            {
                if (ImGui.MenuItem(config.Muted.Contains(c.Key) ? "Unmute" : "Mute"))
                    service.ToggleMute(c.Key);
                if (ImGui.MenuItem("Copy ticket"))
                    ImGui.SetClipboardText(c.Ticket);
                if (ImGui.MenuItem("Leave"))
                {
                    status = service.LeaveChannel(c);
                    if (selected == c.Key)
                        selected = "";
                }

                ImGui.EndPopup();
            }
        }

        if (service.AuthorRoom is not null)
        {
            ImGui.Spacing();
            ImGui.TextColored(Accent, "Author");
            var a = service.AuthorChat();
            if (ImGui.Selectable("Announcements" + (a.Unread > 0 ? $" ({a.Unread})" : "") + "##author", selected == ChatBook.AuthorKey))
                Select(ChatBook.AuthorKey);
            var s = service.SupportChat();
            if (ImGui.Selectable("Write to the author" + (s.Unread > 0 ? $" ({s.Unread})" : "") + "##support", selected == ChatBook.SupportKey))
                Select(ChatBook.SupportKey);
        }
    }

    private void FriendMenu(FriendView f, string key)
    {
        if (!ImGui.BeginPopupContextItem("##fr" + f.Id))
            return;
        if (ImGui.MenuItem(config.Muted.Contains(key) ? "Unmute" : "Mute"))
            service.ToggleMute(key);
        if (service.Channels.Count > 0 && ImGui.BeginMenu("Invite to channel"))
        {
            foreach (var c in service.Channels)
            {
                if (ImGui.MenuItem("#" + ChatText.Label(c.Label) + "##inv" + c.Room))
                    status = service.InviteToChannel(f, c);
            }

            ImGui.EndMenu();
        }

        if (ImGui.MenuItem("Copy id"))
            ImGui.SetClipboardText(f.Id);
        ImGui.Separator();
        if (ImGui.MenuItem("Remove friend (they are told)"))
            status = service.RemoveFriend(f);
        if (ImGui.MenuItem("Block (they are not told)"))
            status = service.Block(f.Id);
        ImGui.EndPopup();
    }

    private void DrawConversation()
    {
        var friend = service.Friends.FirstOrDefault(f => ChatBook.FriendKey(f.Id) == selected);
        var channel = service.Channels.FirstOrDefault(c => c.Key == selected);
        bool author = selected == ChatBook.AuthorKey && service.AuthorRoom is not null;
        bool support = selected == ChatBook.SupportKey && service.AuthorRoom is not null;
        Conversation? chat = friend is not null ? service.FriendChat(friend.Id)
            : channel is not null ? service.ChannelChat(channel)
            : author ? service.AuthorChat()
            : support ? service.SupportChat()
            : null;
        service.OpenConversation = chat?.Key;
        if (chat is null)
        {
            ImGui.TextColored(Dim, service.Friends.Count == 0 ? "Add a friend or make a channel on the Add tab." : "Pick a friend or a channel.");
            return;
        }

        chat.MarkRead();
        ImGui.TextColored(Accent, ChatText.Label(chat.Title));
        if (friend is not null)
        {
            ImGui.SameLine();
            ImGui.TextColored(Dot(friend), friend.StatusLabel + (friend.Online && friend.Note.Length > 0 ? " — " + friend.Note : ""));
        }
        else if (channel is not null)
        {
            ImGui.SameLine();
            ImGui.TextColored(Dim, $"{channel.Members.Count} connected");
        }

        bool canWrite = !author;
        float inputHeight = canWrite ? ImGui.GetFrameHeightWithSpacing() : 0;
        if (ImGui.BeginChild("##lp-lines", new Vector2(0, -ImGui.GetFrameHeightWithSpacing() - inputHeight), true))
        {
            foreach (var line in chat.Lines)
                DrawLine(line);
            if (chat.Version != scrolledVersion)
            {
                ImGui.SetScrollHereY(1f);
                scrolledVersion = chat.Version;
            }
        }

        ImGui.EndChild();
        if (!canWrite)
            return;
        if (focusInput)
        {
            ImGui.SetKeyboardFocusHere();
            focusInput = false;
        }

        ImGui.SetNextItemWidth(-1);
        var hint = support ? "write to the author (they may answer)" : channel is not null ? "say something in #" + channel.Label : "message";
        if (ImGui.InputTextWithHint("##lp-input", hint, ref draft, ChatText.MaxChatBytes, ImGuiInputTextFlags.EnterReturnsTrue))
        {
            status = friend is not null ? service.SendToFriend(friend, draft)
                : channel is not null ? service.SendToChannel(channel, draft)
                : service.WriteToAuthor(draft);
            if (status.Length == 0)
                draft = "";
            focusInput = true;
        }
    }

    private static void DrawLine(ChatLine l)
    {
        var time = l.Time.ToLocalTime().ToString("HH:mm");
        if (l.System)
        {
            ImGui.TextColored(Dim, $"{time}  {l.Text}");
            return;
        }

        var who = l.Outgoing ? "you" : ChatText.Label(l.From);
        var mark = l.State switch
        {
            LineState.Pending => " …",
            LineState.Failed => " ✗",
            _ => "",
        };
        ImGui.TextColored(l.Outgoing ? Dim : Accent, $"{time} {who}{(l.Verified ? " ✓ verified" : "")}{mark}");
        ImGui.SameLine();
        ImGui.PushTextWrapPos(0);
        ImGui.TextUnformatted(l.Text);
        ImGui.PopTextWrapPos();
    }

    private void DrawStatusLine()
    {
        if (status.Length > 0)
            ImGui.TextColored(Dim, status);
        else
            ImGui.TextColored(Dim, $"{service.Friends.Count(f => f.Online)} of {service.Friends.Count} friends online");
    }

    // ---------------------------------------------------------------- add

    private void DrawAdd()
    {
        ImGui.TextColored(Accent, "Invite a friend");
        ImGui.TextWrapped("An invite works once and expires in a day. Send it to them any way you like; whoever uses it first becomes your friend, so do not post it in public.");
        if (ImGui.Button("Make an invite"))
        {
            var (ticket, err) = service.CreateInvite(24 * 3600);
            invite = ticket;
            status = err;
            if (ticket.Length > 0)
                ImGui.SetClipboardText(ticket);
        }

        if (invite.Length > 0)
        {
            ImGui.SameLine();
            if (ImGui.Button("Copy"))
                ImGui.SetClipboardText(invite);
            ImGui.SetNextItemWidth(-1);
            ImGui.InputText("##invite", ref invite, 4096, ImGuiInputTextFlags.ReadOnly);
            ImGui.TextColored(Dim, "Copied to the clipboard.");
        }

        ImGui.Separator();
        ImGui.TextColored(Accent, "Use an invite or a channel ticket");
        ImGui.SetNextItemWidth(-1);
        ImGui.InputTextWithHint("##paste", "paste lpfriend… or lproom…", ref paste, 4096);
        var (kind, _) = Tickets.Find(paste);
        if (paste.Length > 0)
            ImGui.TextColored(kind == TicketKind.None ? Busy : Dim, "That is " + Tickets.Describe(kind) + ".");
        ImGui.BeginDisabled(kind is not (TicketKind.FriendInvite or TicketKind.Room));
        if (ImGui.Button(kind == TicketKind.Room ? "Join this channel" : "Add this friend"))
        {
            status = kind == TicketKind.Room ? service.JoinChannel(paste) : service.AcceptInvite(paste);
            if (status.Length == 0)
            {
                status = kind == TicketKind.Room ? "Joining…" : "Asking them…";
                paste = "";
            }
        }

        ImGui.EndDisabled();

        ImGui.Separator();
        ImGui.TextColored(Accent, "Make a channel");
        ImGui.SetNextItemWidth(220);
        ImGui.InputTextWithHint("##chname", "name, e.g. static", ref channelName, 40);
        ImGui.SameLine();
        if (ImGui.Button("Create"))
        {
            status = service.CreateChannel(channelName);
            if (status.Length == 0)
                channelName = "";
        }

        ImGui.TextColored(Dim, "Anyone with a channel's ticket can join and read it. Invite friends from their right-click menu.");

        if (service.ChannelInvites.Count > 0)
        {
            ImGui.Separator();
            ImGui.TextColored(Accent, "Channel invites");
            foreach (var offer in service.ChannelInvites.ToList())
            {
                ImGui.TextUnformatted(ChatText.Label(offer.FromName) + " invited you to a channel.");
                ImGui.SameLine();
                if (ImGui.SmallButton("Join##" + offer.Ticket[..20]))
                    status = service.JoinChannel(offer.Ticket, "from " + offer.FromName);
                ImGui.SameLine();
                if (ImGui.SmallButton("Ignore##" + offer.Ticket[..20]))
                    service.IgnoreChannelInvite(offer);
            }
        }

        DrawStatusLine();
    }

    // ------------------------------------------------------------- author

    private void DrawAuthor()
    {
        service.OpenConversation = null;
        ImGui.PushTextWrapPos(0);
        if (!AuthorIdentity.Configured)
        {
            ImGui.TextWrapped("This build has no author channel configured yet. When it has one, you can opt in here to hear announcements from the mod's author, and write to them.");
            ImGui.PopTextWrapPos();
            return;
        }

        ImGui.TextWrapped("The author channel carries announcements from the mod's author. They are signed with a key only the author holds, and each is checked before it is shown — nobody else can post there, and there is no chatter. Joining shows your node id and IP address to the members you connect through.");
        bool join = config.JoinAuthorChannel;
        if (ImGui.Checkbox("Join the author channel", ref join))
        {
            config.JoinAuthorChannel = join;
            save();
            status = join ? service.JoinAuthor() : service.LeaveAuthor();
        }

        if (service.AuthorRoom is not null)
        {
            ImGui.Separator();
            foreach (var a in service.Announcements)
            {
                ImGui.TextColored(Accent, $"#{a.Seq} {ChatText.Label(a.Title)}  ✓ verified");
                ImGui.TextColored(Dim, DateTimeOffset.FromUnixTimeSeconds(a.IssuedAt).ToLocalTime().ToString("yyyy-MM-dd HH:mm"));
                if (a.Body.Length > 0)
                    ImGui.TextWrapped(a.Body);
                ImGui.Spacing();
            }

            if (service.Announcements.Count == 0)
                ImGui.TextColored(Dim, "No announcements yet.");
            ImGui.Separator();
            ImGui.TextWrapped("Write to the author (support, feedback). Only what you type is sent, with your node id; they may answer here.");
            ImGui.SetNextItemWidth(-80);
            ImGui.InputTextWithHint("##support", "your message", ref supportDraft, ChatText.MaxChatBytes);
            ImGui.SameLine();
            if (ImGui.Button("Send"))
            {
                status = service.WriteToAuthor(supportDraft);
                if (status.Length == 0)
                {
                    supportDraft = "";
                    Select(ChatBook.SupportKey);
                }
            }
        }

        ImGui.PopTextWrapPos();
        DrawStatusLine();
    }

    // ----------------------------------------------------------- settings

    private void DrawSettings()
    {
        ImGui.TextColored(Accent, service.State == ServiceState.On ? "Linkpearl is on" : "Linkpearl is " + service.State.ToString().ToLowerInvariant());
        if (service.NodeId.Length > 0)
        {
            ImGui.SameLine();
            ImGui.TextColored(Dim, "node " + service.NodeId[..16] + "…");
            ImGui.SameLine();
            if (ImGui.SmallButton("Copy node id"))
                ImGui.SetClipboardText(service.NodeId);
        }

        if (ImGui.Button("Turn Linkpearl off"))
        {
            config.Enabled = false;
            save();
            service.Stop();
        }

        ImGui.SameLine();
        DrawSelfTestButton("Test the connection");
        DrawSelfTestResult();

        ImGui.Separator();
        ImGui.SetNextItemWidth(260);
        ImGui.InputTextWithHint("##name", "name your friends see", ref nameDraft, 64);
        ImGui.SameLine();
        if (ImGui.Button("Set name"))
            status = service.SetName(nameDraft);

        ImGui.Separator();
        DrawRelayChoice();
        ImGui.TextColored(Dim, "Relay changes apply the next time Linkpearl is turned on.");

        ImGui.Separator();
        Toggle("Toasts for messages", () => config.Toasts, v => config.Toasts = v);
        Toggle("Toasts for channel lines", () => config.ChannelToasts, v => config.ChannelToasts = v);
        Toggle("Toast when a friend comes online", () => config.FriendOnlineToasts, v => config.FriendOnlineToasts = v);
        Toggle("Show LP in the server info bar", () => config.ShowDtr, v => config.ShowDtr = v);
        Toggle("Rejoin my channels when Linkpearl starts", () => config.RejoinChannels, v => config.RejoinChannels = v);

        ImGui.Separator();
        ImGui.TextColored(Accent, "Other plugins");
        Toggle("Let other plugins see my friends and incoming messages", () => config.AllowIpcRead, v => config.AllowIpcRead = v);
        Toggle("Let other plugins send messages as me", () => config.AllowIpcSend, v => config.AllowIpcSend = v);

        ImGui.Separator();
        ImGui.TextColored(Accent, "Blocked");
        if (service.Blocked.Count == 0)
            ImGui.TextColored(Dim, "nobody");
        foreach (var id in service.Blocked.ToList())
        {
            ImGui.TextUnformatted(id[..16] + "…");
            ImGui.SameLine();
            if (ImGui.SmallButton("Unblock##" + id))
                status = service.Unblock(id);
        }

        ImGui.Separator();
        ImGui.TextColored(Accent, "Privacy");
        DrawPrivacy();
        ImGui.TextColored(Dim, "Data: " + service.DataPath);
        if (service.Stats is { } s)
            ImGui.TextColored(Dim, $"sent {s.BytesSent / 1024} KiB, received {s.BytesRecv / 1024} KiB, {s.RateLimited} frames dropped by rate limits");
        DrawStatusLine();
    }

    private void Toggle(string label, Func<bool> get, Action<bool> set)
    {
        bool v = get();
        if (ImGui.Checkbox(label, ref v))
        {
            set(v);
            save();
        }
    }
}
