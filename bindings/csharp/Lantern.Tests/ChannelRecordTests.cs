using System.Buffers.Binary;
using System.Text;

namespace Lantern.Tests;

public class ChannelRecordTests
{
    // Structural fixtures only. Native integration tests prove signatures.
    private static LanternEvent Make(string text = "same text", long time = 42, bool history = false)
    {
        var utf8 = Encoding.UTF8.GetBytes(text);
        var bytes = new byte[98 + utf8.Length + 64];
        "LTHREC01"u8.CopyTo(bytes);
        bytes.AsSpan(8, 32).Fill(1);
        bytes.AsSpan(40, 32).Fill(2);
        bytes.AsSpan(72, 16).Fill(3);
        BinaryPrimitives.WriteInt64BigEndian(bytes.AsSpan(88, 8), time);
        BinaryPrimitives.WriteUInt16BigEndian(bytes.AsSpan(96, 2), (ushort)utf8.Length);
        utf8.CopyTo(bytes, 98);
        bytes.AsSpan(bytes.Length - 64).Fill(4);
        return new(history ? LanternEventKind.ChannelHistory : LanternEventKind.ChannelRecord, 1, 7,
            Enumerable.Repeat((byte)1, 32).ToArray(), bytes);
    }

    [Fact]
    public void OriginalMetadataAndFullSignatureArePreserved()
    {
        var e = Make("hello\nworld", 1234, true);
        var record = Assert.IsType<ChannelRecord>(ChannelRecord.FromNativeEvent(e));
        Assert.Equal(string.Concat(Enumerable.Repeat("01", 32)), record.Author);
        Assert.Equal(string.Concat(Enumerable.Repeat("02", 32)), record.Room);
        Assert.Equal(string.Concat(Enumerable.Repeat("03", 16)), record.Id);
        Assert.Equal(string.Concat(Enumerable.Repeat("04", 64)), record.Signature);
        Assert.Equal("hello\nworld", record.Text);
        Assert.Equal(1234, record.ClaimedAt);
        Assert.Equal(DateTimeOffset.FromUnixTimeMilliseconds(1234), record.ClaimedTime);
        Assert.True(record.Historical);
        Assert.False(ChannelRecord.FromNativeEvent(Make())!.Historical);
    }

    [Fact]
    public void UnsafeKindsAuthorsLengthsAndFramingAreRejected()
    {
        var e = Make();
        Assert.Null(ChannelRecord.FromNativeEvent(e with { Kind = LanternEventKind.Message }));
        Assert.Null(ChannelRecord.FromNativeEvent(e with { Peer = new byte[32] }));
        Assert.Null(ChannelRecord.FromNativeEvent(e with { Peer = null! }));
        Assert.Null(ChannelRecord.FromNativeEvent(e with { Data = null! }));
        Assert.Null(ChannelRecord.FromNativeEvent(e with { Data = e.Data[..^1] }));
        Assert.Null(ChannelRecord.FromNativeEvent(e with { Data = [.. e.Data, 0] }));
        var wrongVersion = e.Data.ToArray(); wrongVersion[7] = (byte)'2';
        Assert.Null(ChannelRecord.FromNativeEvent(e with { Data = wrongVersion }));
        Assert.Null(ChannelRecord.FromNativeEvent(Make("")));
        Assert.Null(ChannelRecord.FromNativeEvent(Make(new string('a', 2049))));
        Assert.Null(ChannelRecord.FromNativeEvent(Make("bad\0text")));
        Assert.Null(ChannelRecord.FromNativeEvent(Make("bad\u001btext")));
        var badUtf8 = e.Data.ToArray(); badUtf8[98] = 0xff;
        Assert.Null(ChannelRecord.FromNativeEvent(e with { Data = badUtf8 }));
        Assert.Null(ChannelRecord.FromNativeEvent(Make(time: -1)));
    }

    [Fact]
    public void SenderClockClaimsCannotCrashDateConversion()
    {
        var record = ChannelRecord.FromNativeEvent(Make(time: long.MaxValue))!;
        Assert.Equal(long.MaxValue, record.ClaimedAt);
        Assert.Null(record.ClaimedTime);
        Assert.Null((record with { ClaimedAt = long.MinValue }).ClaimedTime);
        Assert.Equal(2048, ChannelRecord.FromNativeEvent(Make(new string('a', 2048)))!.Text.Length);
    }
}
