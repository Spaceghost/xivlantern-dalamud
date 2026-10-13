using System.Numerics;
using Dalamud.Bindings.ImGui;
using Dalamud.Interface.Windowing;
using Dalamud.Interface;
using XivLantern.Core;

namespace XivLantern.Plugin;

/// <summary>
/// The Lantern window. Off: the privacy note, the relay choice and the switch. On: friends and channels on the
/// left, the open conversation on the right; an Add tab for invites and channels; the author's channel; settings.
/// Nothing here accepts anything on its own: every invite, ticket and channel needs the player's click.
/// </summary>
public sealed class MainWindow : Window, IDisposable
{
    private static readonly Vector4 Accent = new(0.96f, 0.78f, 0.36f, 1f);
    private static readonly Vector4 Good = new(0.45f, 0.86f, 0.58f, 1f);
    private static readonly Vector4 Away = new(0.95f, 0.80f, 0.35f, 1f);
    private static readonly Vector4 Busy = new(1.00f, 0.45f, 0.40f, 1f);
    private static readonly Vector4 Dim = new(0.60f, 0.60f, 0.64f, 1f);
    private static readonly string[] Statuses = ["invisible", "online", "away", "busy"];
    private static readonly string[] RelayNames = ["n0 relays (default)", "Relays off (LAN / direct only)", "My own relay"];

    private readonly LanternService service;
    private readonly Configuration config;
    private readonly Action save;
    private readonly FilePreview filePreview;
    private string selected = "";
    private string draft = "";
    private string fileOfferPath = "";
    private string fileOfferConversation = "";
    private string nameDraft = "";
    private string noteDraft = "";
    private string tagDraft = "";
    private string tagFilter = "";
    private string presetNameDraft = "";
    private string folderNameDraft = "";
    private string invite = "";
    private string paste = "";
    private string channelName = "";
    private bool createSealedChannel;
    private ChannelInfo? resealChannel;
    private readonly HashSet<string> resealFriends = new(StringComparer.OrdinalIgnoreCase);
    private string nostrRecipient = "";
    private string nostrRelayDraft = "";
    private string supportDraft = "";
    private bool focusSupport;
    private string status = "";
    private string[] selftest = [];
    private int scrolledVersion = -1;
    private bool focusInput;
    private bool showSettings;

    public MainWindow(LanternService service, Configuration config, Action save, Dalamud.Plugin.Services.ITextureProvider textures)
        : base("Lantern###XivLantern")
    {
        this.service = service;
        this.config = config;
        this.save = save;
        filePreview = new FilePreview(textures);
        nameDraft = config.DisplayName;
        noteDraft = config.PresenceNote;
        nostrRelayDraft = string.Join("\n", config.NostrRelays ?? []);
        Size = new Vector2(760, 520);
        SizeCondition = ImGuiCond.FirstUseEver;
        SizeConstraints = new WindowSizeConstraints { MinimumSize = new Vector2(480, 320), MaximumSize = new Vector2(float.MaxValue, float.MaxValue) };
        service.Notice += line => status = line;
    }

    public void Dispose() => filePreview.Dispose();

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

        if (!ImGui.BeginTabBar("##lantern-tabs"))
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
        ImGui.TextColored(Accent, "Lantern is off. Nothing is connected.");
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

        ImGui.BeginDisabled(!config.PrivacyAcknowledged || service.SharedCacheBusy || service.State == ServiceState.Stopping);
        if (ImGui.Button("Turn Lantern on"))
        {
            config.Enabled = true;
            save();
            service.Start();
        }

        ImGui.EndDisabled();
        ImGui.SameLine();
        DrawSelfTestButton("Test the connection (temporary node)");
        DrawSelfTestResult();
        ImGui.Separator();
        DrawSharedCacheSettings();

    }

    private void DrawPrivacy()
    {
        ImGui.PushTextWrapPos(0);
        ImGui.TextWrapped(Privacy.Summary);
        ImGui.TextWrapped(Privacy.WhoSeesWhat);
        ImGui.TextWrapped(Privacy.Relay(config.Relay, config.CustomRelayUrl));
        ImGui.TextWrapped(Privacy.NameCards);
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
            ImGui.SetTooltip("Checks that this game can open a connection directly and through the relay. With Lantern off it starts a throwaway node for the test and closes it.");
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
        if (ImGui.BeginChild("##lt-list", new Vector2(listWidth, -ImGui.GetFrameHeightWithSpacing()), true))
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
        int presence = (int)Math.Min(service.EffectivePresence, 3);
        ImGui.SetNextItemWidth(110);
        if (ImGui.Combo("##presence", ref presence, Statuses, Statuses.Length))
            status = service.SetPresence((uint)presence, noteDraft);
        ImGui.SameLine();
        ImGui.SetNextItemWidth(260);
        if (ImGui.InputTextWithHint("##note", "a note for friends (optional)", ref noteDraft, 120, ImGuiInputTextFlags.EnterReturnsTrue))
            status = service.SetPresence(config.Presence, noteDraft);
        ImGui.SameLine();
        ImGui.TextColored(Dim, config.DisplayName.Length > 0 ? "as " + ChatText.Label(config.DisplayName) : "(no name set)");
        DrawActivityTags();
        ImGui.SetNextItemWidth(170);
        if (ImGui.BeginCombo("##statusPresets", "Status presets"))
        {
            foreach (var preset in config.StatusPresets)
            {
                ImGui.PushID(preset.Name);
                if (ImGui.Selectable(ChatText.Label(preset.Name)))
                {
                    status = ApplyPreset(preset.Name);
                    presetNameDraft = preset.Name;
                }
                if (ImGui.IsItemHovered()) ImGui.SetTooltip(Statuses[(int)preset.Presence] + (preset.Note.Length > 0 ? " — " + ChatText.Label(preset.Note) : ""));
                ImGui.PopID();
            }
            if (config.StatusPresets.Count == 0) ImGui.TextDisabled("No saved presets yet");
            ImGui.EndCombo();
        }
        ImGui.SameLine();
        if (ImGui.Button("Manage presets")) ImGui.OpenPopup("statusPresetEditor");
        if (ImGui.BeginPopup("statusPresetEditor"))
        {
            ImGui.TextWrapped("Save the current presence and note field as a preset. Choose it from the list to apply it.");
            ImGui.InputText("Preset name", ref presetNameDraft, 128);
            if (ImGui.Button("Save preset"))
            {
                config.StatusPresets = StatusPresets.Save(config.StatusPresets, presetNameDraft, service.EffectivePresence, noteDraft, out var error);
                status = error.Length == 0 ? "Preset saved." : error;
                if (error.Length == 0) save();
            }
            ImGui.SameLine();
            if (ImGui.Button("Delete preset"))
            {
                var exists = StatusPresets.Find(config.StatusPresets, presetNameDraft) != null;
                if (exists) { config.StatusPresets = StatusPresets.Remove(config.StatusPresets, presetNameDraft); save(); }
                status = exists ? "Preset deleted." : "No preset with that name.";
            }
            ImGui.EndPopup();
        }
    }

    public string ApplyPreset(string name)
    {
        var preset = StatusPresets.Find(config.StatusPresets, name);
        if (preset == null) return "No saved status preset with that name.";
        var error = service.SetPresence(preset.Presence, preset.Note);
        if (error.Length == 0) noteDraft = preset.Note;
        return error;
    }

    private void DrawActivityTags()
    {
        ImGui.TextDisabled("Up for:");
        if (service.IsOn && !service.SupportsActivityTags)
        {
            ImGui.TextDisabled("Update Lantern to share activities.");
            return;
        }
        foreach (var tag in ActivityTags.Suggested)
        {
            ImGui.SameLine();
            var enabled = config.ActivityTags.Contains(tag, StringComparer.OrdinalIgnoreCase);
            if (ImGui.Checkbox(tag + "##activity", ref enabled))
                ChangeActivity(tag, enabled);
        }
        foreach (var tag in config.ActivityTags.Where(t => !ActivityTags.Suggested.Contains(t, StringComparer.OrdinalIgnoreCase)).ToList())
        {
            ImGui.PushID("activity:" + tag);
            if (ImGui.SmallButton(ChatText.Label(tag) + " ×")) ChangeActivity(tag, false);
            ImGui.PopID();
            ImGui.SameLine();
        }
        ImGui.SetNextItemWidth(170);
        var add = ImGui.InputTextWithHint("##activityText", "your own activity", ref tagDraft, 128, ImGuiInputTextFlags.EnterReturnsTrue);
        ImGui.SameLine();
        if (ImGui.SmallButton("Add activity")) add = true;
        if (add)
        {
            var tag = ActivityTags.Normalize([tagDraft]).FirstOrDefault();
            if (tag == null) status = "Type an activity first.";
            else if (ChangeActivity(tag, true)) tagDraft = "";
        }
        if (ImGui.IsItemHovered()) ImGui.SetTooltip("Up to eight activities, each at most 32 UTF-8 bytes. Only what you choose or type is shared.");
    }

    private bool ChangeActivity(string tag, bool enabled)
    {
        var tags = config.ActivityTags.Where(t => !t.Equals(tag, StringComparison.OrdinalIgnoreCase)).ToList();
        if (enabled)
        {
            if (tags.Count >= ActivityTags.MaxTags) { status = "Remove an activity before adding another (maximum eight)."; return false; }
            tags.Add(tag);
        }
        status = service.SetActivityTags(tags);
        return status.Length == 0;
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
        foreach (var tag in ActivityTags.Suggested)
        {
            if (ImGui.SmallButton((tag.Equals(tagFilter, StringComparison.OrdinalIgnoreCase) ? "[" + tag + "]" : tag) + "##filterChip"))
                tagFilter = tag.Equals(tagFilter, StringComparison.OrdinalIgnoreCase) ? "" : tag;
            if (ImGui.GetContentRegionAvail().X > 80) ImGui.SameLine();
        }
        ImGui.NewLine();
        if (ImGui.BeginCombo("##activityFilter", tagFilter.Length == 0 ? "Any activity" : ChatText.Label(tagFilter)))
        {
            if (ImGui.Selectable("Any activity", tagFilter.Length == 0)) tagFilter = "";
            foreach (var tag in ActivityTags.Suggested.Concat(service.Friends.Where(f => f.Online).SelectMany(f => f.Tags))
                .Distinct(StringComparer.OrdinalIgnoreCase).OrderBy(t => t, StringComparer.OrdinalIgnoreCase))
            {
                ImGui.PushID("filter:" + tag);
                if (ImGui.Selectable(ChatText.Label(tag), tag.Equals(tagFilter, StringComparison.OrdinalIgnoreCase))) tagFilter = tag;
                ImGui.PopID();
            }
            ImGui.EndCombo();
        }
        var visible = service.Friends.Where(f => ActivityTags.Matches(f, tagFilter)).ToList();
        if (service.Friends.Count == 0)
            ImGui.TextColored(Dim, "none yet — see Add");
        else if (visible.Count == 0) ImGui.TextDisabled("No online friends with that activity.");
        foreach (var folder in FriendFolders.Group(visible, config.FriendFolders))
        {
            ImGui.PushID("friend-folder:" + folder.Name);
            var open = ImGui.CollapsingHeader((folder.Name.Length == 0 ? "Ungrouped" : ChatText.Label(folder.Name)) + $" {folder.Online}/{folder.Friends.Count}###folderHeader", ImGuiTreeNodeFlags.DefaultOpen);
            if (ImGui.BeginPopupContextItem("folderActions"))
            {
                var wholeFolder = FriendFolders.Group(service.Friends, config.FriendFolders).First(g => g.Name.Equals(folder.Name, StringComparison.OrdinalIgnoreCase));
                var allMuted = wholeFolder.Friends.All(f => config.Muted.Contains(ChatBook.FriendKey(f.Id)));
                if (ImGui.MenuItem(allMuted ? "Unmute folder" : "Mute folder"))
                {
                    config.Muted = FriendFolders.SetMuted(config.Muted, wholeFolder, !allMuted);
                    save();
                }
                ImGui.EndPopup();
            }
            if (open) foreach (var f in folder.Friends) DrawFriend(f);
            ImGui.PopID();
        }

        ImGui.Spacing();
        ImGui.TextColored(Accent, "Channels");
        DrawChannels();
    }

    private void DrawFriend(FriendView f)
    {
        var key = ChatBook.FriendKey(f.Id);
        var chat = service.Book.Find(key);
        int unread = chat?.Unread ?? 0;
        ImGui.TextColored(Dot(f), "●");
        ImGui.SameLine();
        var label = ChatText.Label(f.Display) + (SafetyWords.IsVerified(config.VerifiedFriends, service.NodeId, f.Id) ? " · verified" : "") + (unread > 0 ? $" ({unread})" : "") + (config.Muted.Contains(key) ? " · muted" : "") + "##" + f.Id;
        if (ImGui.Selectable(label, selected == key))
        {
            Select(key);
            service.EnsureHistory(f);
        }

        if (ImGui.IsItemHovered())
            ImGui.SetTooltip($"{f.StatusLabel}{(f.Online && f.Note.Length > 0 ? " — " + f.Note : "")}{(f.Online && f.Tags.Count > 0 ? "\nUp for: " + string.Join(", ", f.Tags) : "")}\n{f.Id}");
        FriendMenu(f, key);
    }

    private void DrawChannels()
    {
        bool openReseal = false;
        if (service.Channels.Count == 0)
            ImGui.TextColored(Dim, "none — see Add");
        foreach (var c in service.Channels.ToList())
        {
            var chat = service.ChannelChat(c);
            if (c.Sealed) { DrawSealedIndicator(); ImGui.SameLine(); }
            var label = "#" + ChatText.Label(c.Label) + (chat.Unread > 0 ? $" ({chat.Unread})" : "") + (config.Muted.Contains(c.Key) ? " · muted" : "") + "##" + c.Room;
            if (ImGui.Selectable(label, selected == c.Key))
                Select(c.Key);
            if (ImGui.BeginPopupContextItem("##ch" + c.Room))
            {
                if (ImGui.MenuItem(config.Muted.Contains(c.Key) ? "Unmute" : "Mute"))
                    service.ToggleMute(c.Key);
                if (c.Sealed && service.SupportsSealedChannels && ImGui.MenuItem("Reseal with selected friends…"))
                {
                    resealChannel = c;
                    resealFriends.Clear();
                    openReseal = true;
                }
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

        if (openReseal) ImGui.OpenPopup("Reseal channel");
        if (ImGui.BeginPopup("Reseal channel"))
        {
            ImGui.TextWrapped("Create a new channel and key. Only selected friends receive its ticket. The old channel remains; old tickets still read old history.");
            ImGui.TextWrapped("Resealing #" + ChatText.Clean(resealChannel?.Label ?? "unavailable channel"));
            var eligibleFriends = service.Friends.Where(f => f.Linked
                && !service.Blocked.Contains(f.Id, StringComparer.OrdinalIgnoreCase)).ToList();
            foreach (var unavailable in resealFriends.Where(id => !eligibleFriends.Any(f =>
                string.Equals(f.Id, id, StringComparison.OrdinalIgnoreCase))).ToList())
            {
                ImGui.TextWrapped("Selected friend is no longer available: " + ChatText.Preview(service.NameOf(unavailable), 64));
                ImGui.SameLine();
                if (ImGui.SmallButton("Remove selection##reseal-unavailable" + unavailable))
                    resealFriends.Remove(unavailable);
            }
            foreach (var friend in eligibleFriends)
            {
                bool chosen = resealFriends.Contains(friend.Id);
                if (ImGui.Checkbox(ChatText.Label(friend.Display) + "##reseal" + friend.Id, ref chosen))
                {
                    if (chosen) resealFriends.Add(friend.Id); else resealFriends.Remove(friend.Id);
                }
            }
            ImGui.BeginDisabled(resealChannel == null || !service.Channels.Contains(resealChannel));
            if (ImGui.Button("Create new key and invite selected"))
            {
                int before = service.Channels.Count;
                status = service.ResealChannel(resealChannel!, resealFriends.ToArray());
                if (service.Channels.Count > before)
                {
                    Select(service.Channels.Last().Key);
                    resealChannel = null;
                    resealFriends.Clear();
                    ImGui.CloseCurrentPopup();
                }
            }
            ImGui.EndDisabled();
            ImGui.SameLine();
            if (ImGui.Button("Cancel"))
            {
                resealChannel = null;
                resealFriends.Clear();
                ImGui.CloseCurrentPopup();
            }
            ImGui.EndPopup();
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
        if (ImGui.BeginMenu("Move to folder"))
        {
            if (ImGui.MenuItem("Ungrouped")) AssignFolder(f, "");
            foreach (var folder in FriendFolders.Group(service.Friends, config.FriendFolders).Where(g => g.Name.Length > 0))
                if (ImGui.MenuItem(ChatText.Label(folder.Name))) AssignFolder(f, folder.Name);
            ImGui.Separator();
            ImGui.InputText("New folder", ref folderNameDraft, 128);
            if (ImGui.Button("Assign folder"))
            {
                if (string.IsNullOrWhiteSpace(folderNameDraft)) status = "Choose a folder name.";
                else AssignFolder(f, folderNameDraft);
            }
            ImGui.EndMenu();
        }
        if (service.Channels.Count > 0 && ImGui.BeginMenu("Invite to channel", f.Linked
            && !service.Blocked.Contains(f.Id, StringComparer.OrdinalIgnoreCase)))
        {
            foreach (var c in service.Channels)
            {
                if (c.Sealed) { DrawSealedIndicator(); ImGui.SameLine(); }
                if (ImGui.MenuItem("#" + ChatText.Label(c.Label) + "##inv" + c.Room))
                    status = service.InviteToChannel(f, c);
                if (c.Sealed && ImGui.IsItemHovered())
                    ImGui.SetTooltip("Send this friend the private channel ticket. It grants access to messages, retained history and shared files.");
            }

            ImGui.EndMenu();
        }

        if (ImGui.MenuItem("Copy id"))
            ImGui.SetClipboardText(f.Id);
        if (ImGui.BeginMenu("Compare safety words"))
        {
            if (SafetyWords.Compare(service.NodeId, f.Id) is { } comparison)
            {
                ImGui.TextUnformatted(comparison.Words);
                ImGui.TextWrapped("Compare these words in a separate, trusted conversation. Mark compared only after they match.");
                if (ImGui.MenuItem("Copy words")) ImGui.SetClipboardText(comparison.Words);
                if (ImGui.MenuItem("Copy full comparison fingerprint")) ImGui.SetClipboardText(comparison.Fingerprint);
                var verified = SafetyWords.IsVerified(config.VerifiedFriends, service.NodeId, f.Id);
                if (ImGui.MenuItem(verified ? "Clear comparison mark" : "Mark compared"))
                {
                    if (verified) config.VerifiedFriends.Remove(f.Id.ToLowerInvariant());
                    else config.VerifiedFriends[f.Id.ToLowerInvariant()] = comparison.Fingerprint;
                    save();
                }
            }
            else ImGui.TextDisabled("Node identities are not available.");
            ImGui.EndMenu();
        }
        ImGui.Separator();
        if (ImGui.MenuItem("Remove friend (they are told)"))
            status = service.RemoveFriend(f);
        if (ImGui.MenuItem("Block (they are not told)"))
            status = service.Block(f.Id);
        ImGui.EndPopup();
    }

    private void AssignFolder(FriendView friend, string name)
    {
        config.FriendFolders = FriendFolders.Assign(config.FriendFolders, friend.Id, name, out var error);
        status = error;
        if (error.Length == 0) save();
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
        if (channel?.Sealed == true) { DrawSealedIndicator(); ImGui.SameLine(); }
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
            if (service.SupportsChannelHistory)
            {
                ImGui.SameLine();
                ImGui.BeginDisabled(!config.ChannelHistoryEnabled);
                if (ImGui.SmallButton("Catch up")) status = service.RefreshChannelHistory(channel);
                ImGui.EndDisabled();
                ImGui.SameLine();
                if (ImGui.SmallButton("Clear local history")) status = service.ClearChannelHistory(channel);
                if (ImGui.IsItemHovered()) ImGui.SetTooltip("Other members keep their copies. A later catch-up can restore them.");
            }
        }

        bool canWrite = !author;
        float inputHeight = canWrite ? ImGui.GetFrameHeightWithSpacing() : 0;
        if (ImGui.BeginChild("##lt-lines", new Vector2(0, -ImGui.GetFrameHeightWithSpacing() - inputHeight), true))
        {
            foreach (var line in chat.Lines)
                DrawLine(line);
            if (channel is not null) DrawFileOffers(channel);
            else if (friend is not null) DrawFileOffers(null, friend);
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
        if (ImGui.InputTextWithHint("##lt-input", hint, ref draft, ChatText.MaxChatBytes, ImGuiInputTextFlags.EnterReturnsTrue))
        {
            status = friend is not null ? service.SendToFriend(friend, draft)
                : channel is not null ? service.SendToChannel(channel, draft)
                : service.WriteToAuthor(draft);
            if (status.Length == 0)
                draft = "";
            focusInput = true;
        }
    }

    private void DrawFileOffers(ChannelInfo? channel, FriendView? friend = null)
    {
        ImGui.BeginDisabled(service.FileSendBusy || !service.SupportsFileDownloads
            || (friend != null && (!service.SupportsFriendFiles || !friend.Linked)));
        if (ImGui.SmallButton("Share a file…"))
        {
            fileOfferConversation = channel?.Key ?? ChatBook.FriendKey(friend!.Id);
            ImGui.OpenPopup("Share file");
        }
        ImGui.EndDisabled();
        if (ImGui.BeginPopup("Share file"))
        {
            if (fileOfferConversation != (channel?.Key ?? ChatBook.FriendKey(friend!.Id)))
            {
                ImGui.CloseCurrentPopup();
                ImGui.EndPopup();
                return;
            }
            ImGui.TextWrapped("Offer a file to " + (channel != null ? "#" + ChatText.Clean(channel.Label) : ChatText.Clean(friend!.Display))
                + ". Recipients choose whether to download it.");
            ImGui.TextWrapped("Screenshots may include character names and chat. Check the file before sharing. Maximum 16 MiB.");
            ImGui.InputTextWithHint("##filePath", "full local file path", ref fileOfferPath, 4096);
            if (ImGui.Button("Offer this file"))
            {
                status = channel != null ? service.OfferFile(channel, fileOfferPath) : service.OfferFriendFile(friend!, fileOfferPath);
                if (status.Length == 0) ImGui.CloseCurrentPopup();
            }
            ImGui.SameLine();
            if (ImGui.Button("Cancel")) ImGui.CloseCurrentPopup();
            ImGui.EndPopup();
        }
        if (service.FileSendStatus.Length > 0) ImGui.TextWrapped(service.FileSendStatus);
        var outgoingKey = channel?.Key ?? ChatBook.FriendKey(friend!.Id);
        foreach (var sent in service.OutgoingFiles.Where(o => o.Key == outgoingKey).ToList())
        {
            ImGui.PushID("outgoing-file:" + sent.Handle);
            ImGui.Separator();
            ImGui.TextWrapped("You offered " + ChatText.Preview(sent.Name, 128) + $" ({sent.Size / 1024.0:F1} KiB)");
            if (ImGui.SmallButton("Withdraw offer")) status = service.WithdrawFileOffer(sent);
            if (ImGui.IsItemHovered()) ImGui.SetTooltip("Stop offering this file. This does not erase copies already downloaded or clear the local shared-file cache.");
            ImGui.PopID();
        }
        foreach (var offer in service.FileOffers.All.Where(o => channel != null ? o.Room == channel.Room
            : o.Room == 0 && o.Peer == friend!.Id).ToList())
        {
            ImGui.PushID("file:" + offer.Handle);
            ImGui.Separator();
            ImGui.TextWrapped(ChatText.Preview(service.NameOf(offer.Peer), 64) + " offered " + offer.Name + $" ({offer.Size / 1024.0:F1} KiB)");
            if (offer.State is FileOfferState.Offered or FileOfferState.Failed or FileOfferState.Canceled)
            {
                ImGui.TextDisabled("Download only files you trust. Nothing opens automatically.");
                ImGui.BeginDisabled(!service.SupportsFileDownloads);
                if (ImGui.SmallButton(offer.State == FileOfferState.Offered ? "Download" : "Retry download")) status = service.DownloadFile(offer.Handle);
                ImGui.EndDisabled();
                if (!service.SupportsFileDownloads) ImGui.TextDisabled("Update Lantern to download shared files.");
            }
            else if (offer.State == FileOfferState.Fetching)
            {
                ImGui.TextDisabled("Downloading…");
                ImGui.SameLine();
                if (ImGui.SmallButton("Cancel")) status = service.CancelFileDownload(offer.Handle);
            }
            else if (offer.State == FileOfferState.Canceling) ImGui.TextDisabled("Canceling…");
            else
            {
                ImGui.TextDisabled(offer.Withdrawn ? "Offer withdrawn; your saved copy remains." : "Saved locally.");
                if (ImGui.SmallButton("Copy saved path")) ImGui.SetClipboardText(offer.Destination);
                filePreview.Draw(offer);
            }
            ImGui.PopID();
        }
        if (service.FileDownloadStatus.Length > 0) ImGui.TextWrapped(service.FileDownloadStatus);
    }

    private static void DrawSealedIndicator()
    {
        ImGui.PushFont(UiBuilder.IconFont);
        ImGui.TextColored(Accent, FontAwesomeIcon.Lock.ToIconString());
        ImGui.PopFont();
        if (ImGui.IsItemHovered())
            ImGui.SetTooltip("Sealed channel: messages, shared history and files are encrypted. Anyone with its ticket can read them. Local saved data is not encrypted.");
    }

    private static void DrawLine(ChatLine l)
    {
        var time = l.ClaimedAt is < 0 or > 253402300799999L ? "--:--" : l.Time.ToLocalTime().ToString("HH:mm");
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
        if (l.SignedAuthor is not null && ImGui.IsItemHovered())
            ImGui.SetTooltip($"Signature verified for node:\n{l.SignedAuthor}\nThe sender's timestamp is a claim and can be wrong.");
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

    private void DrawNameCards()
    {
        ImGui.TextColored(Accent, "Name-card invitations");
        ImGui.TextWrapped("Use a public nostr name card to invite a friend while they are offline. Chosen relays learn your connection and message timing. The local identity key is stored unencrypted in Lantern's config folder.");
        var enabled = config.NostrInvitesEnabled;
        if (ImGui.Checkbox("Enable name-card invitations", ref enabled))
        {
            service.SetNostrInvitesEnabled(enabled);
        }
        ImGui.TextWrapped("Turning this off stops new requests and cancels name lookup. A relay operation already queued can still finish.");
        if (!enabled) return;
        if (!service.SupportsNostrInvites)
        {
            ImGui.TextWrapped("This Lantern build does not include name-card support. Ordinary tickets still work above.");
            return;
        }
        ImGui.InputTextMultiline("Relays (one ws/wss URL per line)", ref nostrRelayDraft, 16385, new Vector2(-1, 70));
        if (ImGui.Button("Save chosen relays"))
        {
            var relays = nostrRelayDraft.Split('\n', StringSplitOptions.RemoveEmptyEntries).Select(x => x.Trim()).Distinct().ToList();
            if (relays.Count is < 1 or > 8 || relays.Any(x => System.Text.Encoding.UTF8.GetByteCount(x) > 2048
                || x.Any(c => char.IsWhiteSpace(c) || char.IsControl(c))
                || !Uri.TryCreate(x, UriKind.Absolute, out var uri) || string.IsNullOrEmpty(uri.Host)
                || uri.Scheme is not ("ws" or "wss")))
                status = "Choose one to eight valid WebSocket relay URLs.";
            else { config.NostrRelays = relays; save(); status = "Chosen relays saved."; }
        }
        ImGui.BeginDisabled(service.NostrBusy);
        if (ImGui.Button("Prepare my name card")) status = service.PrepareNostrIdentity();
        if (service.NostrNpub.Length > 0)
        {
            ImGui.SameLine();
            if (ImGui.Button("Copy my npub")) ImGui.SetClipboardText(service.NostrNpub);
            ImGui.TextWrapped(service.NostrNpub);
        }
        ImGui.SetNextItemWidth(-1);
        ImGui.InputTextWithHint("##namecardrecipient", "name@domain, npub, nprofile or public hex key", ref nostrRecipient, 1025);
        if (ImGui.Button("Resolve name")) status = service.ResolveNostrName(nostrRecipient);
        if (nostrRecipient == service.NostrResolvedName && service.NostrResolvedKey.Length > 0)
        {
            ImGui.TextWrapped("The domain maps this name to " + service.NostrResolvedKey);
            ImGui.TextWrapped("This identifies a public key; it does not prove who the person is. Sending uses this key even if the domain mapping later changes.");
        }
        ImGui.BeginDisabled(config.NostrRelays.Count == 0);
        if (ImGui.Button("Send one-day invite")) status = service.SendNostrInvite(nostrRecipient);
        ImGui.SameLine();
        if (ImGui.Button("Check invitations")) status = service.CheckNostrInbox();
        ImGui.EndDisabled();
        ImGui.EndDisabled();
        if (service.NostrStatus.Length > 0) ImGui.TextWrapped(service.NostrStatus);
        foreach (var offer in service.NameCardInbox.Pending.ToList())
        {
            ImGui.PushID(offer.Key);
            ImGui.TextWrapped("Invitation from nostr " + offer.Sender);
            ImGui.BeginDisabled(offer.AcceptHandle != 0);
            if (ImGui.Button("Accept")) status = service.AcceptNostrInvite(offer.Key);
            ImGui.SameLine();
            if (ImGui.Button("Ignore")) service.IgnoreNostrInvite(offer.Key);
            ImGui.EndDisabled();
            if (offer.AcceptHandle != 0) ImGui.TextColored(Dim, "Asking them…");
            ImGui.PopID();
        }
    }

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
        ImGui.InputTextWithHint("##paste", "paste ltfriend…, ltroom… or ltsealed…", ref paste, 4096);
        var (kind, _) = Tickets.Find(paste);
        if (paste.Length > 0)
            ImGui.TextColored(kind == TicketKind.None ? Busy : Dim, "That is " + Tickets.Describe(kind) + ".");
        ImGui.BeginDisabled(kind is not (TicketKind.FriendInvite or TicketKind.Room or TicketKind.SealedRoom));
        if (ImGui.Button(kind is TicketKind.Room or TicketKind.SealedRoom ? "Join this channel" : "Add this friend"))
        {
            status = kind is TicketKind.Room or TicketKind.SealedRoom ? service.JoinChannel(paste) : service.AcceptInvite(paste);
            if (status.Length == 0)
            {
                status = kind is TicketKind.Room or TicketKind.SealedRoom ? "Joining…" : "Asking them…";
                paste = "";
            }
        }

        ImGui.EndDisabled();

        ImGui.Separator();
        DrawNameCards();
        ImGui.Separator();
        ImGui.TextColored(Accent, "Make a channel");
        ImGui.SetNextItemWidth(220);
        ImGui.InputTextWithHint("##chname", "name, e.g. static", ref channelName, 40);
        ImGui.SameLine();
        if (ImGui.Button("Create"))
        {
            status = service.CreateChannel(channelName, createSealedChannel);
            if (status.Length == 0)
                channelName = "";
        }

        ImGui.BeginDisabled(!service.SupportsSealedChannels);
        ImGui.Checkbox("Seal channel (encrypt messages, history and files)", ref createSealedChannel);
        ImGui.EndDisabled();
        if (!service.SupportsSealedChannels) ImGui.TextDisabled("Update Lantern to create sealed channels.");
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
                ImGui.TextColored(Dim, a.IssuedAt is >= -62135596800 and <= 253402300799
                    ? DateTimeOffset.FromUnixTimeSeconds(a.IssuedAt).ToLocalTime().ToString("yyyy-MM-dd HH:mm") : "Time unavailable");
                if (a.Body.Length > 0)
                    ImGui.TextWrapped(a.Body);
                if (ImGui.SmallButton("Write to the author##announcement:" + a.Seq)) focusSupport = true;
                ImGui.Spacing();
            }

            if (service.Announcements.Count == 0)
                ImGui.TextColored(Dim, "No announcements yet.");
            ImGui.Separator();
            ImGui.TextWrapped("Write to the author (support, feedback). Only what you type is sent, with your node id; they may answer here.");
            ImGui.SetNextItemWidth(-80);
            if (focusSupport) ImGui.SetKeyboardFocusHere();
            ImGui.InputTextWithHint("##support", "your message", ref supportDraft, ChatText.MaxChatBytes);
            if (focusSupport) { ImGui.SetScrollHereY(); focusSupport = false; }
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

    private void DrawSharedCacheSettings()
    {
        var budget = Math.Clamp(config.SharedCacheBudgetMiB, 64, 1024);
        if (ImGui.SliderInt("Shared-file cache (MiB)", ref budget, 64, 1024)) { config.SharedCacheBudgetMiB = budget; save(); }
        ImGui.TextWrapped("Checks cached storage before new uploads, with room reserved for file data and metadata. Saved downloads have a separate budget. Turn Lantern off to clear cached copies.");
        ImGui.BeginDisabled(!service.CanClearSharedCache);
        if (ImGui.Button("Clear shared-file cache…")) ImGui.OpenPopup("Clear shared-file cache?");
        ImGui.EndDisabled();
        if (ImGui.BeginPopup("Clear shared-file cache?"))
        {
            ImGui.TextWrapped("Remove Lantern's cached copies of shared files? Your originals, saved downloads, friends and identity stay intact.");
            ImGui.BeginDisabled(!service.CanClearSharedCache);
            if (ImGui.Button("Clear cached copies")) { status = service.ClearSharedFileCache(); ImGui.CloseCurrentPopup(); }
            ImGui.EndDisabled();
            ImGui.SameLine(); if (ImGui.Button("Cancel")) ImGui.CloseCurrentPopup();
            ImGui.EndPopup();
        }
        if (service.SharedCacheCloseError) ImGui.TextWrapped("The cache did not close cleanly. Reload Lantern before clearing cached copies.");
        if (service.SharedCacheStatus.Length > 0) ImGui.TextWrapped(service.SharedCacheStatus);
    }

    private void DrawSettings()
    {
        ImGui.TextColored(Accent, service.State == ServiceState.On ? "Lantern is on" : "Lantern is " + service.State.ToString().ToLowerInvariant());
        if (service.NodeId.Length > 0)
        {
            ImGui.SameLine();
            ImGui.TextColored(Dim, "node " + service.NodeId[..16] + "…");
            ImGui.SameLine();
            if (ImGui.SmallButton("Copy node id"))
                ImGui.SetClipboardText(service.NodeId);
        }

        if (ImGui.Button("Turn Lantern off"))
        {
            config.Enabled = false;
            save();
            service.Stop();
        }

        ImGui.SameLine();
        DrawSelfTestButton("Test the connection");
        DrawSelfTestResult();

        ImGui.Separator();
        DrawSharedCacheSettings();
        var downloadBudget = Math.Clamp(config.DownloadBudgetMiB, 32, 1024);
        if (ImGui.SliderInt("Download storage (MiB)", ref downloadBudget, 32, 1024))
        {
            config.DownloadBudgetMiB = downloadBudget;
            save();
        }
        ImGui.TextWrapped("The budget applies to new downloads and reserves space for two transfers.");
        var retention = Math.Clamp(config.DownloadRetentionDays, 0, 90);
        if (ImGui.SliderInt("Remove old downloads after (days)", ref retention, 0, 90))
        {
            config.DownloadRetentionDays = retention;
            save();
        }
        ImGui.TextWrapped("Zero keeps downloads. When enabled, Lantern removes its older downloaded files before preparing a new download; active transfers are preserved.");

        ImGui.Separator();
        ImGui.SetNextItemWidth(260);
        ImGui.InputTextWithHint("##name", "name your friends see", ref nameDraft, 64);
        ImGui.SameLine();
        if (ImGui.Button("Set name"))
            status = service.SetName(nameDraft);

        ImGui.Separator();
        DrawRelayChoice();
        ImGui.TextColored(Dim, "Relay changes apply the next time Lantern is turned on.");

        ImGui.Separator();
        Toggle("Toasts for messages", () => config.Toasts, v => config.Toasts = v);
        Toggle("Toasts for channel lines", () => config.ChannelToasts, v => config.ChannelToasts = v);
        Toggle("Toast when a friend comes online", () => config.FriendOnlineToasts, v => config.FriendOnlineToasts = v);
        Toggle("Show Lantern in the server info bar", () => config.ShowDtr, v => config.ShowDtr = v);
        Toggle("Catch up and share recent channel messages", () => config.ChannelHistoryEnabled, v => status = service.SetChannelHistory(v));
        ImGui.TextWrapped("Signed channel lines are kept locally for up to 24 hours, with 256 per channel. Others keep copies; clearing locally does not delete theirs. Sender timestamps can be wrong. Recent channel contacts may be remembered for up to 30 days to help reconnect.");
        if (service.IsOn && !service.SupportsChannelHistory) ImGui.TextColored(Dim, "Channel catch-up needs the updated native library.");
        Toggle("Show Lantern channel pull timers", () => config.PullTimersEnabled, v => config.PullTimersEnabled = v);
        Toggle("Play a notification chime at zero", () => config.PullTimerChime, v => config.PullTimerChime = v);
        Toggle("Hide friend names and notes in the server-bar tooltip", () => config.HideDtrRoster, v => config.HideDtrRoster = v);
        Toggle("Automatically show away after keyboard/mouse inactivity", () => config.IdleAwayEnabled, v => config.IdleAwayEnabled = v);
        int idleMinutes = config.IdleAwayMinutes;
        if (ImGui.InputInt("Away after (minutes)", ref idleMinutes)) { config.IdleAwayMinutes = Math.Clamp(idleMinutes, 1, 120); save(); }
        ImGui.TextWrapped("Only changes online presence. Restores your presence and note when input resumes. Controller-only play may not register as activity under Wine.");
        Toggle("Rejoin my channels when Lantern starts", () => config.RejoinChannels, v => config.RejoinChannels = v);

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
