using Dalamud.Plugin;
using Dalamud.Plugin.Ipc;
using XivLinkpearl.Core;
using XivLinkpearl.Shared;

namespace XivLinkpearl.Plugin;

/// <summary>
/// The gates in docs/IPC.md. Reading friends and messages, and sending as the player, are refused until the player
/// allows each in settings; every gate answers "error: …" instead of throwing into the caller.
/// </summary>
public sealed class IpcService : IDisposable
{
    private readonly LinkpearlService service;
    private readonly Configuration config;
    private readonly Action<string> openChat;
    private readonly ICallGateProvider<string> status;
    private readonly ICallGateProvider<string> friends;
    private readonly ICallGateProvider<string, string, string> send;
    private readonly ICallGateProvider<string, object> open;
    private readonly ICallGateProvider<string, object> received;

    public IpcService(IDalamudPluginInterface pi, LinkpearlService service, Configuration config, Action<string> openChat)
    {
        this.service = service;
        this.config = config;
        this.openChat = openChat;
        status = pi.GetIpcProvider<string>(IpcContract.GetStatus);
        status.RegisterFunc(Status);
        friends = pi.GetIpcProvider<string>(IpcContract.GetFriends);
        friends.RegisterFunc(Friends);
        send = pi.GetIpcProvider<string, string, string>(IpcContract.SendToFriend);
        send.RegisterFunc(Send);
        open = pi.GetIpcProvider<string, object>(IpcContract.OpenChat);
        open.RegisterAction(who => openChat(who ?? ""));
        received = pi.GetIpcProvider<string, object>(IpcContract.MessageReceived);
        service.Arrived += OnArrived;
    }

    private string Status() => IpcPayloads.Status(config.Enabled, service.IsOn, service.NodeId, config.DisplayName,
        service.Friends.Count(f => f.Online), service.Book.TotalUnread);

    private string Friends()
    {
        if (!config.AllowIpcRead)
            return "error: the player has not allowed other plugins to read Linkpearl";
        return service.IsOn ? IpcPayloads.Friends(service.Friends) : "error: Linkpearl is off";
    }

    private string Send(string who, string text)
    {
        if (!config.AllowIpcSend)
            return "error: the player has not allowed other plugins to send on Linkpearl";
        if (!service.IsOn)
            return "error: Linkpearl is off";
        var (friend, err) = FriendLookup.Find(service.Friends, who ?? "");
        if (friend is null)
            return "error: " + err;
        if (string.IsNullOrWhiteSpace(text))
            return "error: empty message";
        var result = service.SendToFriend(friend, text);
        return result.Length == 0 ? "ok" : "error: " + result;
    }

    private void OnArrived(Incoming what, string conversation, string from, string text)
    {
        if (!config.AllowIpcRead)
            return;
        var kind = what switch
        {
            Incoming.FriendText => "friend",
            Incoming.SupportReply => "author_reply",
            Incoming.Announcement => "announcement",
            _ => null,
        };
        if (kind is null)
            return;
        var id = conversation.StartsWith("f:", StringComparison.Ordinal) ? conversation[2..] : "";
        try
        {
            received.SendMessage(IpcPayloads.Message(kind, id, from, text));
        }
        catch
        {
            // A subscriber's failure is theirs.
        }
    }

    public void Dispose()
    {
        service.Arrived -= OnArrived;
        status.UnregisterFunc();
        friends.UnregisterFunc();
        send.UnregisterFunc();
        open.UnregisterAction();
    }
}
