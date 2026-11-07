using System.Buffers.Binary;
using System.Text;

namespace Lantern;

/// <summary>Original metadata from a cryptographically verified native record event.
/// Parsing checks structure, not cryptography; do not use it to verify arbitrary external bytes.</summary>
public sealed record ChannelRecord(string Author, string Room, string Id, long ClaimedAt,
    string Text, string Signature, bool Historical)
{
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);

    /// <summary>Sender time is a claim. Out-of-range claims remain in ClaimedAt but have no display date.</summary>
    public DateTimeOffset? ClaimedTime => ClaimedAt is >= 0 and <= 253402300799999L
        ? DateTimeOffset.FromUnixTimeMilliseconds(ClaimedAt) : null;

    public static ChannelRecord? FromNativeEvent(LanternEvent e)
    {
        if (e.Kind is not (LanternEventKind.ChannelRecord or LanternEventKind.ChannelHistory)) return null;
        if (e.Data is null || e.Peer is null) return null;
        var raw = e.Data.AsSpan();
        if (e.Peer.Length != 32 || raw.Length is < 163 or > 2210 || !raw[..8].SequenceEqual("LTHREC01"u8)) return null;
        if (!raw.Slice(8, 32).SequenceEqual(e.Peer)) return null;
        var length = BinaryPrimitives.ReadUInt16BigEndian(raw.Slice(96, 2));
        var time = BinaryPrimitives.ReadInt64BigEndian(raw.Slice(88, 8));
        if (length is 0 or > 2048 || raw.Length != 98 + length + 64 || time < 0) return null;
        string text;
        try { text = StrictUtf8.GetString(raw.Slice(98, length)); }
        catch (DecoderFallbackException) { return null; }
        if (text.Any(c => char.IsControl(c) && c != '\n')) return null;
        return new ChannelRecord(Convert.ToHexStringLower(raw.Slice(8, 32)),
            Convert.ToHexStringLower(raw.Slice(40, 32)), Convert.ToHexStringLower(raw.Slice(72, 16)),
            time, text, Convert.ToHexStringLower(raw[^64..]), e.Kind == LanternEventKind.ChannelHistory);
    }
}
