namespace XivLantern.Core;

public enum Verb
{
    Toggle,
    Help,
    Settings,
    On,
    Off,
    SelfTest,
    Invite,
    Accept,
    Status,
    Msg,
    Unknown,
}

/// <summary>One parsed <c>/lantern</c> command line.</summary>
public sealed record Command(Verb Verb, string Arg = "", string Text = "")
{
    public const string Name = "/lantern";

    /// <summary>The same command under the mod's full name, for when another plugin already has /lantern.</summary>
    public const string Alias = "/xivlantern";

    public const string Help =
        "/lantern — open or close the window. Subcommands: settings, on, off, selftest, invite (copies a friend invite), " +
        "accept <ltfriend…>, status <online|away|busy|invisible> [note], msg <friend> <text>, help.";

    public static Command Parse(string? arguments)
    {
        var a = (arguments ?? "").Trim();
        if (a.Length == 0)
            return new Command(Verb.Toggle);
        var (head, rest) = Split(a);
        return head.ToLowerInvariant() switch
        {
            "help" or "?" => new Command(Verb.Help),
            "settings" or "config" => new Command(Verb.Settings),
            "on" or "enable" => new Command(Verb.On),
            "off" or "disable" => new Command(Verb.Off),
            "selftest" or "test" => new Command(Verb.SelfTest),
            "invite" => new Command(Verb.Invite, rest),
            "accept" => new Command(Verb.Accept, rest),
            "status" => ParseStatus(rest),
            "msg" or "tell" => ParseMsg(rest),
            _ => new Command(Verb.Unknown, head),
        };
    }

    private static Command ParseStatus(string rest)
    {
        var (status, note) = Split(rest);
        return new Command(Verb.Status, status.ToLowerInvariant(), note);
    }

    private static Command ParseMsg(string rest)
    {
        var (who, text) = Split(rest);
        return new Command(Verb.Msg, who, text);
    }

    private static (string Head, string Tail) Split(string s)
    {
        s = s.Trim();
        int i = s.IndexOfAny([' ', '\t']);
        return i < 0 ? (s, "") : (s[..i], s[(i + 1)..].Trim());
    }

    /// <summary>LT_PRESENCE_* for a status word, or null.</summary>
    public static uint? PresenceOf(string word) => word switch
    {
        "invisible" or "hidden" => 0,
        "online" => 1,
        "away" or "afk" => 2,
        "busy" or "dnd" => 3,
        _ => null,
    };
}
