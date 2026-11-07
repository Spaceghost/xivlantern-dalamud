using System.Security.Cryptography;
using System.Text;

namespace XivLantern.Core;

public sealed record NostrPendingInvite(string Key, string Sender, string Ticket, ulong AcceptHandle = 0);

/// <summary>Local pending invitations. Native verifies the sender/ticket; this model
/// bounds UI state and remembers explicit ignores without persisting bearer tickets.</summary>
public sealed class NostrInbox
{
    public const int Capacity = 32;
    public const int SenderCapacity = 4;
    public const int IgnoreCapacity = 256;
    public const int SenderWindowLimit = 8;
    public const int WindowLimit = 64;
    public const long WindowMilliseconds = 30000;
    private readonly List<NostrPendingInvite> pending = [];
    private readonly List<string> ignored = [];
    private readonly Func<long> clock;
    private readonly Dictionary<string, int> admissions = [];
    private long windowStart = -1;
    private long lastNow;
    private int windowCount;
    public IReadOnlyList<NostrPendingInvite> Pending => pending;
    public IReadOnlyList<string> IgnoredKeys => ignored;

    public NostrInbox(IEnumerable<string>? ignoredKeys = null, Func<long>? monotonicMilliseconds = null)
    {
        clock = monotonicMilliseconds ?? (() => Environment.TickCount64);
        foreach (var key in ignoredKeys ?? [])
            if (IsKey(key) && !ignored.Contains(key.ToLowerInvariant()))
            {
                ignored.Add(key.ToLowerInvariant());
                if (ignored.Count > IgnoreCapacity) ignored.RemoveAt(0);
            }
    }

    private static bool IsKey(string? key) => key is { Length: 64 }
        && key.All(c => c is >= '0' and <= '9' or >= 'a' and <= 'f' or >= 'A' and <= 'F');

    public bool Add(string sender, string ticket)
    {
        if (!IsKey(sender) || string.IsNullOrEmpty(ticket) || ticket.Length > 4096) return false;
        var (kind, canonical) = Tickets.Find(ticket);
        if (kind != TicketKind.FriendInvite || !string.Equals(ticket, canonical, StringComparison.OrdinalIgnoreCase)) return false;
        sender = sender.ToLowerInvariant();
        var key = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes("Lantern pending invite v1\0" + sender + canonical)));
        if (ignored.Contains(key) || pending.Any(p => p.Key == key) || pending.Count >= Capacity
            || pending.Count(p => p.Sender == sender) >= SenderCapacity) return false;
        // Removing/ignoring a pending line must not bypass the time-window limit.
        var now = Math.Max(lastNow, clock());
        lastNow = now;
        if (windowStart < 0 || now - windowStart >= WindowMilliseconds)
        {
            windowStart = now;
            windowCount = 0;
            admissions.Clear();
        }
        var count = admissions.GetValueOrDefault(sender);
        if (windowCount >= WindowLimit || count >= SenderWindowLimit) return false;
        admissions[sender] = count + 1;
        windowCount++;
        pending.Add(new(key, sender, canonical));
        return true;
    }

    public NostrPendingInvite? Find(string key) => pending.FirstOrDefault(p => p.Key == key);

    public bool AcceptStarted(string key, ulong handle)
    {
        var index = pending.FindIndex(p => p.Key == key && p.AcceptHandle == 0);
        if (index < 0 || handle == 0 || pending.Any(p => p.AcceptHandle == handle)) return false;
        pending[index] = pending[index] with { AcceptHandle = handle };
        return true;
    }

    /// <summary>Only successful native acknowledgement removes an accepted invitation.</summary>
    public bool Complete(ulong handle, bool success)
    {
        if (handle == 0) return false;
        var index = pending.FindIndex(p => p.AcceptHandle == handle);
        if (index < 0) return false;
        if (success) { Remember(pending[index].Key); pending.RemoveAt(index); }
        else pending[index] = pending[index] with { AcceptHandle = 0 };
        return true;
    }

    public bool Ignore(string key)
    {
        var index = pending.FindIndex(p => p.Key == key && p.AcceptHandle == 0);
        if (index < 0) return false;
        Remember(key);
        pending.RemoveAt(index);
        return true;
    }

    private void Remember(string key)
    {
        if (ignored.Contains(key)) return;
        ignored.Add(key);
        if (ignored.Count > IgnoreCapacity) ignored.RemoveAt(0);
    }

    public void ClearPending() => pending.Clear();
}
