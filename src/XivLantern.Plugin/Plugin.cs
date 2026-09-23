using Dalamud.Game.Command;
using Dalamud.Interface.Windowing;
using Dalamud.Plugin;
using Dalamud.Plugin.Services;
using XivLantern.Core;

namespace XivLantern.Plugin;

/// <summary>
/// /lantern: a friend list, 1:1 chat and channels over iroh, peer to peer, plus an opt-in channel for the mod's
/// author. Off until the player turns it on; nothing about the player's character is read from the game.
/// </summary>
public sealed class Plugin : IDalamudPlugin
{
    private readonly IDalamudPluginInterface pluginInterface;
    private readonly IPluginLog log;
    private readonly ICommandManager commands;
    private readonly IChatGui chat;
    private readonly WindowSystem windowSystem = new("XivLantern");
    private readonly Configuration config = null!;
    private readonly LanternService service = null!;
    private readonly MainWindow window = null!;
    private readonly Notifier? notifier;
    private readonly IpcService? ipc;
    private bool commandRegistered;
    private bool aliasRegistered;

    public Plugin(IDalamudPluginInterface pluginInterface, IPluginLog log, IFramework framework, ICommandManager commands, IChatGui chat,
        INotificationManager notifications, IDtrBar dtrBar)
    {
        this.pluginInterface = pluginInterface;
        this.log = log;
        this.commands = commands;
        this.chat = chat;
        try
        {
            config = pluginInterface.GetPluginConfig() as Configuration ?? new Configuration();
            // Dalamud loads the managed assembly from memory; lantern.dll is found by the folder it was installed to.
            var nativeDir = pluginInterface.AssemblyLocation.DirectoryName ?? pluginInterface.ConfigDirectory.FullName;
            pluginInterface.ConfigDirectory.Create();
            service = new LanternService(nativeDir, pluginInterface.ConfigDirectory.FullName, config, Save, log, framework);
            window = new MainWindow(service, config, Save);
            windowSystem.AddWindow(window);
            notifier = new Notifier(notifications, dtrBar, framework, log, config, service, window.Toggle);
            ipc = new IpcService(pluginInterface, service, config, who => window.OpenChat(who));
            pluginInterface.UiBuilder.Draw += DrawUi;
            pluginInterface.UiBuilder.OpenMainUi += window.Toggle;
            pluginInterface.UiBuilder.OpenConfigUi += window.ShowSettings;
            commandRegistered = commands.AddHandler(Command.Name, new CommandInfo(OnCommand) { HelpMessage = Command.Help });
            aliasRegistered = commands.AddHandler(Command.Alias, new CommandInfo(OnCommand) { HelpMessage = "Same as " + Command.Name + ".", ShowInHelp = false });

            if (config.Enabled && config.PrivacyAcknowledged)
                service.Start();
        }
        catch
        {
            // Dalamud does not call Dispose when the constructor throws; release what was acquired.
            DisposeCore();
            throw;
        }
    }

    private void Save() => pluginInterface.SavePluginConfig(config);

    private void Print(string line) => chat.Print("[Lantern] " + line);

    private void DrawUi()
    {
        try
        {
            windowSystem.Draw();
        }
        catch (Exception ex)
        {
            log.Error(ex, "Lantern UI draw failed");
        }
    }

    private void OnCommand(string command, string arguments)
    {
        try
        {
            var c = Command.Parse(arguments);
            switch (c.Verb)
            {
                case Verb.Toggle:
                    window.Toggle();
                    break;
                case Verb.Help:
                    Print(Command.Help);
                    break;
                case Verb.Settings:
                    window.ShowSettings();
                    break;
                case Verb.On:
                    if (!config.PrivacyAcknowledged)
                    {
                        Print("Read the privacy note first: it opens now. Nothing has been connected.");
                        window.IsOpen = true;
                        break;
                    }

                    config.Enabled = true;
                    Save();
                    service.Start();
                    Print("starting…");
                    break;
                case Verb.Off:
                    config.Enabled = false;
                    Save();
                    service.Stop();
                    Print("off. Nothing is connected.");
                    break;
                case Verb.SelfTest:
                    Print(service.IsOn ? "testing this node (up to a minute)…" : "Lantern is off: testing with a temporary node that is closed afterwards (up to a minute)…");
                    service.SelfTest((report, raw) =>
                    {
                        if (report is null)
                        {
                            Print("selftest failed: " + raw);
                            return;
                        }

                        foreach (var line in report.Lines())
                            Print(line);
                        Print(report.Passed(config.Relay != RelayChoice.Off) ? "selftest passed." : "selftest found a problem (see above).");
                        window.ShowSelfTest(report.Lines());
                    });
                    break;
                case Verb.Invite:
                {
                    var (ticket, err) = service.CreateInvite(24 * 3600);
                    if (ticket.Length == 0)
                    {
                        Print(err);
                        break;
                    }

                    Dalamud.Bindings.ImGui.ImGui.SetClipboardText(ticket);
                    Print("a one-time friend invite (valid for a day) is on your clipboard. Send it privately.");
                    break;
                }
                case Verb.Accept:
                {
                    var err = service.AcceptInvite(c.Arg);
                    Print(err.Length == 0 ? "asking them…" : err);
                    break;
                }
                case Verb.Status:
                {
                    if (Command.PresenceOf(c.Arg) is not { } p)
                    {
                        Print("status is one of online, away, busy, invisible");
                        break;
                    }

                    var err = service.SetPresence(p, c.Text);
                    Print(err.Length == 0 ? "status: " + c.Arg : err);
                    break;
                }
                case Verb.Msg:
                {
                    var (friend, why) = FriendLookup.Find(service.Friends, c.Arg);
                    var err = friend is null ? why : service.SendToFriend(friend, c.Text);
                    Print(err.Length == 0 ? "sent." : err);
                    break;
                }
                default:
                    Print("unknown: " + c.Arg + ". " + Command.Help);
                    break;
            }
        }
        catch (Exception ex)
        {
            log.Error(ex, "{Command} failed", Command.Name);
        }
    }

    public void Dispose() => DisposeCore();

    private void DisposeCore()
    {
        if (commandRegistered)
            commands.RemoveHandler(Command.Name);
        if (aliasRegistered)
            commands.RemoveHandler(Command.Alias);
        commandRegistered = aliasRegistered = false;
        pluginInterface.UiBuilder.Draw -= DrawUi;
        if (window is not null)
        {
            pluginInterface.UiBuilder.OpenMainUi -= window.Toggle;
            pluginInterface.UiBuilder.OpenConfigUi -= window.ShowSettings;
        }

        ipc?.Dispose();
        notifier?.Dispose();
        windowSystem.RemoveAllWindows();
        service?.Dispose();
    }
}
