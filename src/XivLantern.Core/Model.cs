using System.Text.Json;

namespace XivLantern.Core;

/// <summary>One friend device, as lt_friend_list describes it.</summary>
public sealed record FriendView(string Id, string Name, bool Online, string Status, string Note, long? LastSeen, bool Linked, long FriendId)
{
    public byte[] Key => Convert.FromHexString(Id);

    public string Display => string.IsNullOrWhiteSpace(Name) ? Id[..10] : Name;

    public string StatusLabel => Online ? Status : "offline";
}

public sealed record HistoryLine(ulong Id, bool Outgoing, string Text, long SentAt, long? DeliveredAt);

public sealed record Announcement(ulong Seq, long IssuedAt, string Title, string Body);

public sealed record Probe(bool Ok, long Ms, string Detail);

/// <summary>What lt_selftest reports.</summary>
public sealed record SelfTestReport(string NodeId, string Platform, IReadOnlyList<string> Bound, string RelayUrl, Probe Direct, Probe Relay)
{
    public IEnumerable<string> Lines()
    {
        yield return $"node id {NodeId} ({Platform})";
        yield return "bound " + (Bound.Count == 0 ? "nothing" : string.Join(", ", Bound));
        yield return Direct.Ok ? $"direct path: ok ({Direct.Ms} ms)" : $"direct path: FAILED — {Direct.Detail}";
        yield return Relay.Ok
            ? $"relay path: ok via {RelayUrl} ({Relay.Ms} ms)"
            : Relay.Detail == "relays are off" ? "relay path: not tried (relays are off)" : $"relay path: FAILED — {Relay.Detail}";
    }

    public bool Passed(bool relaysOn) => Direct.Ok && (!relaysOn || Relay.Ok);
}

/// <summary>Parses the JSON the native layer hands out. Bad input gives an empty result, never an exception.</summary>
public static class NativeJson
{
    public static IReadOnlyList<FriendView> Friends(string json) => Array(json, e => new FriendView(
        Str(e, "id"), ChatText.Clean(Str(e, "name")), Bool(e, "online"), Str(e, "status"), ChatText.Clean(Str(e, "note")),
        NullableLong(e, "last_seen"), Bool(e, "linked"), Long(e, "friend")));

    public static IReadOnlyList<HistoryLine> History(string json) => Array(json, e => new HistoryLine(
        ULong(e, "id"), Bool(e, "outgoing"), Str(e, "text"), Long(e, "sent_at"), NullableLong(e, "delivered_at")));

    public static IReadOnlyList<Announcement> Announcements(string json) => Array(json, AnnouncementOf);

    public static IReadOnlyList<string> Strings(string json) => Array(json, e => e.GetString() ?? "");

    public static Announcement? AnnouncementEvent(string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            return AnnouncementOf(doc.RootElement);
        }
        catch (JsonException)
        {
            return null;
        }
    }

    public static SelfTestReport? SelfTest(string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var r = doc.RootElement;
            return new SelfTestReport(Str(r, "node_id"), Str(r, "platform"),
                r.TryGetProperty("bound", out var b) && b.ValueKind == JsonValueKind.Array ? b.EnumerateArray().Select(x => x.GetString() ?? "").ToList() : [],
                Str(r, "relay_url"), ProbeOf(r, "direct"), ProbeOf(r, "relay"));
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static Announcement AnnouncementOf(JsonElement e) =>
        new(ULong(e, "seq"), Long(e, "issued_at"), ChatText.Clean(Str(e, "title")), ChatText.Clean(Str(e, "body")));

    private static Probe ProbeOf(JsonElement r, string name) =>
        r.TryGetProperty(name, out var p) && p.ValueKind == JsonValueKind.Object
            ? new Probe(Bool(p, "ok"), Long(p, "ms"), Str(p, "detail"))
            : new Probe(false, 0, "missing");

    private static IReadOnlyList<T> Array<T>(string json, Func<JsonElement, T> map)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            return doc.RootElement.ValueKind == JsonValueKind.Array ? doc.RootElement.EnumerateArray().Select(map).ToList() : [];
        }
        catch (Exception e) when (e is JsonException or InvalidOperationException or FormatException)
        {
            return [];
        }
    }

    private static string Str(JsonElement e, string name) =>
        e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() ?? "" : "";

    private static bool Bool(JsonElement e, string name) =>
        e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.True;

    private static long Long(JsonElement e, string name) =>
        e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt64(out var n) ? n : 0;

    private static ulong ULong(JsonElement e, string name) =>
        e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetUInt64(out var n) ? n : 0;

    private static long? NullableLong(JsonElement e, string name) =>
        e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt64(out var n) ? n : null;
}

/// <summary>Finds a friend by what a player types: an exact name (any case), or a unique id prefix.</summary>
public static class FriendLookup
{
    public static (FriendView? Friend, string Error) Find(IReadOnlyList<FriendView> friends, string who)
    {
        who = who.Trim();
        if (who.Length == 0)
            return (null, "which friend?");
        var byName = friends.Where(f => string.Equals(f.Name, who, StringComparison.OrdinalIgnoreCase)).ToList();
        if (byName.Count == 1)
            return (byName[0], "");
        var prefix = who.ToLowerInvariant();
        var byId = friends.Where(f => f.Id.StartsWith(prefix, StringComparison.Ordinal)).ToList();
        if (byId.Count == 1)
            return (byId[0], "");
        return byName.Count + byId.Count == 0 ? (null, $"no friend called \"{who}\"") : (null, $"\"{who}\" matches more than one friend; use their id");
    }
}
