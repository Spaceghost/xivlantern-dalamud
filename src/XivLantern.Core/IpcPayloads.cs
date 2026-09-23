using System.Text.Json;

namespace XivLantern.Core;

/// <summary>The JSON the IPC gates answer with (docs/IPC.md). Built with Utf8JsonWriter so text is always escaped.</summary>
public static class IpcPayloads
{
    public static string Status(bool enabled, bool online, string nodeId, string name, int friendsOnline, int unread) =>
        Write(w =>
        {
            w.WriteStartObject();
            w.WriteBoolean("enabled", enabled);
            w.WriteBoolean("online", online);
            w.WriteString("node_id", nodeId);
            w.WriteString("name", name);
            w.WriteNumber("friends_online", friendsOnline);
            w.WriteNumber("unread", unread);
            w.WriteEndObject();
        });

    public static string Friends(IEnumerable<FriendView> friends) =>
        Write(w =>
        {
            w.WriteStartArray();
            foreach (var f in friends)
            {
                w.WriteStartObject();
                w.WriteString("id", f.Id);
                w.WriteString("name", f.Name);
                w.WriteBoolean("online", f.Online);
                w.WriteString("status", f.StatusLabel);
                w.WriteString("note", f.Online ? f.Note : "");
                w.WriteEndObject();
            }

            w.WriteEndArray();
        });

    /// <param name="kind">"friend", "author_reply" or "announcement".</param>
    public static string Message(string kind, string from, string name, string text) =>
        Write(w =>
        {
            w.WriteStartObject();
            w.WriteString("kind", kind);
            w.WriteString("from", from);
            w.WriteString("name", name);
            w.WriteString("text", text);
            w.WriteEndObject();
        });

    private static string Write(Action<Utf8JsonWriter> body)
    {
        using var ms = new MemoryStream();
        using (var w = new Utf8JsonWriter(ms))
            body(w);
        return System.Text.Encoding.UTF8.GetString(ms.ToArray());
    }
}
