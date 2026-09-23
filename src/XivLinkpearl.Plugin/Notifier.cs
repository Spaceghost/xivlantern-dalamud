using Dalamud.Game.Gui.Dtr;
using Dalamud.Interface.ImGuiNotification;
using Dalamud.Plugin.Services;
using XivLinkpearl.Core;

namespace XivLinkpearl.Plugin;

/// <summary>
/// Toasts for what arrives, and a server info bar entry ("LP 3") with the unread count. Both only while Linkpearl is
/// on, and both follow the player's settings and mutes (<see cref="NotifyPolicy"/>).
/// </summary>
public sealed class Notifier : IDisposable
{
    private const string Title = "XivLinkpearl";
    private static readonly TimeSpan Interval = TimeSpan.FromMilliseconds(500);

    private readonly INotificationManager notifications;
    private readonly IDtrBar dtrBar;
    private readonly IFramework framework;
    private readonly IPluginLog log;
    private readonly Configuration config;
    private readonly LinkpearlService service;
    private readonly Action toggleWindow;
    private IDtrBarEntry? entry;
    private DateTime nextUpdate;
    private string lastText = "";
    private bool dtrFailed;

    public Notifier(INotificationManager notifications, IDtrBar dtrBar, IFramework framework, IPluginLog log, Configuration config, LinkpearlService service, Action toggleWindow)
    {
        this.notifications = notifications;
        this.dtrBar = dtrBar;
        this.framework = framework;
        this.log = log;
        this.config = config;
        this.service = service;
        this.toggleWindow = toggleWindow;
        service.Arrived += OnArrived;
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
                Title = "Linkpearl · " + ChatText.Preview(from, 40),
                Content = ChatText.Preview(text, 120),
                Type = what == Incoming.Announcement ? NotificationType.Info : NotificationType.None,
                InitialDuration = TimeSpan.FromSeconds(6),
            });
        }
        catch (Exception ex)
        {
            log.Debug(ex, "Linkpearl: toast failed");
        }
    }

    private void OnUpdate(IFramework _)
    {
        if (dtrFailed)
            return;
        var now = DateTime.UtcNow;
        if (now < nextUpdate)
            return;
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
            int online = service.Friends.Count(f => f.Online);
            var text = service.State switch
            {
                ServiceState.On => unread > 0 ? $"LP ✉{unread}" : $"LP {online}",
                ServiceState.Starting => "LP …",
                ServiceState.Failed => "LP !",
                _ => "LP ○",
            };
            var tip = service.State == ServiceState.On
                ? $"Linkpearl: {online} friend(s) online, {unread} unread. Click to open."
                : $"Linkpearl is {service.State.ToString().ToLowerInvariant()}. Click to open.";
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
            log.Warning(ex, "Linkpearl: server info bar entry disabled after an error");
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
        framework.Update -= OnUpdate;
        Remove();
    }
}
