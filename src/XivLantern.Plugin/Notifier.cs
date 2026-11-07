using Dalamud.Game.Gui.Dtr;
using Dalamud.Interface.ImGuiNotification;
using Dalamud.Plugin.Services;
using XivLantern.Core;

namespace XivLantern.Plugin;

/// <summary>
/// Toasts for what arrives, and a server info bar entry ("Lantern 3") with the unread count. Both only while Lantern is
/// on, and both follow the player's settings and mutes (<see cref="NotifyPolicy"/>).
/// </summary>
public sealed class Notifier : IDisposable
{
    private const string Title = "XivLantern";
    private static readonly TimeSpan Interval = TimeSpan.FromMilliseconds(500);

    private readonly INotificationManager notifications;
    private readonly IDtrBar dtrBar;
    private readonly IFramework framework;
    private readonly IPluginLog log;
    private readonly Configuration config;
    private readonly LanternService service;
    private readonly Action toggleWindow;
    private IDtrBarEntry? entry;
    private DateTime nextUpdate;
    private string lastText = "";
    private bool dtrFailed;
    private bool? lastHideRoster;

    public Notifier(INotificationManager notifications, IDtrBar dtrBar, IFramework framework, IPluginLog log, Configuration config, LanternService service, Action toggleWindow)
    {
        this.notifications = notifications;
        this.dtrBar = dtrBar;
        this.framework = framework;
        this.log = log;
        this.config = config;
        this.service = service;
        this.toggleWindow = toggleWindow;
        service.Arrived += OnArrived;
        service.PullCompleted += OnPullCompleted;
        framework.Update += OnUpdate;
    }

    private void OnArrived(Incoming what, string conversation, string from, string text)
    {
        if (!NotifyPolicy.Toast(what, conversation, service.NotifySettings, service.OpenConversation))
            return;
        try
        {
            notifications.AddNotification(new Notification
            {
                Title = "Lantern · " + ChatText.Preview(from, 40),
                Content = ChatText.Preview(text, 120),
                Type = what == Incoming.Announcement ? NotificationType.Info : NotificationType.None,
                InitialDuration = TimeSpan.FromSeconds(6),
            });
        }
        catch (Exception ex)
        {
            log.Debug(ex, "Lantern: toast failed");
        }
    }

    private void OnPullCompleted(string channel)
    {
        if (!config.PullTimersEnabled) return;
        if (config.PullTimerChime) PullChime.Play();
        if (!config.Toasts) return;
        try
        {
            notifications.AddNotification(new Notification
            {
                Title = "Lantern pull", Content = "Go · #" + ChatText.Preview(channel, 60),
                Type = NotificationType.Success, InitialDuration = TimeSpan.FromSeconds(3),
            });
        }
        catch { /* A notification failure must not interrupt network polling. */ }
    }

    private void OnUpdate(IFramework _)
    {
        if (dtrFailed)
            return;
        var now = DateTime.UtcNow;
        if (now < nextUpdate && lastHideRoster == config.HideDtrRoster)
            return;
        lastHideRoster = config.HideDtrRoster;
        nextUpdate = now + Interval;
        try
        {
            if (!config.ShowDtr || !config.Enabled)
            {
                Remove();
                return;
            }

            if (entry is null)
            {
                entry = dtrBar.Get(Title);
                entry.OnClick = _ =>
                {
                    try
                    {
                        toggleWindow();
                    }
                    catch
                    {
                        // Click handlers run inside game UI callbacks.
                    }
                };
                lastText = "";
            }

            int unread = service.Book.TotalUnread;
            var roster = DtrRoster.Build(service.Friends, unread, config.HideDtrRoster);
            var text = service.State switch
            {
                ServiceState.On => roster.Text,
                ServiceState.Starting => "Lantern …",
                ServiceState.Failed => "Lantern !",
                _ => "Lantern ○",
            };
            var tip = service.State == ServiceState.On
                ? roster.Tooltip
                : $"Lantern is {service.State.ToString().ToLowerInvariant()}. Click to open.";
            if (text + tip == lastText)
                return;
            lastText = text + tip;
            entry.Text = text;
            entry.Tooltip = tip;
            entry.Shown = true;
        }
        catch (Exception ex)
        {
            dtrFailed = true;
            log.Warning(ex, "Lantern: server info bar entry disabled after an error");
            Remove();
        }
    }

    private void Remove()
    {
        try
        {
            entry?.Remove();
        }
        catch
        {
            // Already gone.
        }

        entry = null;
        lastText = "";
    }

    public void Dispose()
    {
        service.Arrived -= OnArrived;
        service.PullCompleted -= OnPullCompleted;
        framework.Update -= OnUpdate;
        Remove();
    }
}
