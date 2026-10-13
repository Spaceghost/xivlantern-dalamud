using System.Buffers.Binary;
using System.IO.Compression;

namespace XivLantern.Core;

/// <summary>Conservative admission for local preview snapshots. This is not an image
/// decoder: malformed pixel data may still be rejected by the platform decoder.
/// Strip ancillary metadata; admit only static non-interlaced PNG and single-scan
/// baseline JPEG with bounded dimensions, before invoking that decoder.</summary>
public static class ImagePreview
{
    public const int MaxSide = 4096;
    public const int MaxPixels = 8 * 1024 * 1024;
    public static byte[] Admit(ReadOnlySpan<byte> bytes)
    {
        if (bytes.Length > (int)FileOffers.MaxBytes) throw Invalid();
        if (bytes.StartsWith(new byte[] {137, 80, 78, 71, 13, 10, 26, 10})) return Png(bytes);
        if (bytes.StartsWith(new byte[] {255, 216})) return Jpeg(bytes);
        throw Invalid();
    }
    private static IOException Invalid() => new("Preview supports static PNG or baseline JPEG up to 4096 pixels per side and 8 megapixels.");
    private static void Dimensions(uint width, uint height)
    {
        if (width == 0 || height == 0 || width > MaxSide || height > MaxSide || (ulong)width * height > MaxPixels) throw Invalid();
    }
    private static byte[] Png(ReadOnlySpan<byte> bytes)
    {
        using var output = new MemoryStream(); output.Write(bytes[..8]);
        using var compressed = new MemoryStream();
        var width = 0; var height = 0; var channels = 0;
        var pos = 8; var header = false; var data = false; var endedData = false;
        while (pos <= bytes.Length - 12)
        {
            var length = BinaryPrimitives.ReadUInt32BigEndian(bytes[pos..]);
            if (length > (uint)(bytes.Length - pos - 12)) throw Invalid();
            var chunk = bytes.Slice(pos, (int)length + 12);
            var type = chunk.Slice(4, 4); var body = chunk.Slice(8, (int)length);
            if (!header && !type.SequenceEqual("IHDR"u8)) throw Invalid();
            if (type.SequenceEqual("IHDR"u8))
            {
                if (header || length != 13) throw Invalid();
                Dimensions(BinaryPrimitives.ReadUInt32BigEndian(body), BinaryPrimitives.ReadUInt32BigEndian(body[4..]));
                width = (int)BinaryPrimitives.ReadUInt32BigEndian(body); height = (int)BinaryPrimitives.ReadUInt32BigEndian(body[4..]);
                channels = body[9] switch { 0 or 3 => 1, 2 => 3, 4 => 2, 6 => 4, _ => 0 };
                // Restrict to the common 8-bit encodings; no Adam7 or animation.
                if (body[8] != 8 || body[9] is not (0 or 2 or 3 or 4 or 6) || body[10] != 0 || body[11] != 0 || body[12] != 0) throw Invalid();
                header = true;
            }
            else if (type.SequenceEqual("IDAT"u8)) { if (endedData) throw Invalid(); data = true; compressed.Write(body); }
            else if (type.SequenceEqual("IEND"u8))
            {
                if (!data || length != 0 || pos + chunk.Length != bytes.Length) throw Invalid();
                compressed.Position = 0;
                using var decoded = new ZLibStream(compressed, CompressionMode.Decompress);
                var row = new byte[width * channels + 1];
                for (var y = 0; y < height; y++)
                {
                    try { decoded.ReadExactly(row); } catch (EndOfStreamException) { throw Invalid(); }
                    if (row[0] > 4) throw Invalid();
                }
                if (decoded.ReadByte() != -1) throw Invalid();
                output.Write(chunk); return output.ToArray();
            }
            else
            {
                if (data) endedData = true;
                if (type.SequenceEqual("acTL"u8) || type.SequenceEqual("fcTL"u8) || type.SequenceEqual("fdAT"u8)) throw Invalid();
                if (type.SequenceEqual("PLTE"u8)) { if (data || length == 0 || length > 768 || length % 3 != 0) throw Invalid(); }
                else if (type.SequenceEqual("tRNS"u8)) { if (data || length > 256) throw Invalid(); }
                else { if ((type[0] & 32) == 0) throw Invalid(); pos += chunk.Length; continue; }
            }
            output.Write(chunk); pos += chunk.Length;
        }
        throw Invalid();
    }
    private static byte[] Jpeg(ReadOnlySpan<byte> bytes)
    {
        using var output = new MemoryStream(); output.Write(bytes[..2]);
        var pos = 2; var frame = false;
        while (pos < bytes.Length - 3)
        {
            var start = pos;
            if (bytes[pos++] != 255) throw Invalid();
            while (pos < bytes.Length && bytes[pos] == 255) pos++;
            if (pos >= bytes.Length) throw Invalid();
            var marker = bytes[pos++];
            if (pos > bytes.Length - 2) throw Invalid();
            var length = BinaryPrimitives.ReadUInt16BigEndian(bytes[pos..]);
            if (length < 2 || length > bytes.Length - pos) throw Invalid();
            var body = bytes.Slice(pos + 2, length - 2); pos += length;
            if (marker == 0xC0)
            {
                if (frame || body.Length < 6 || body[0] != 8 || body[5] is not (1 or 3) || body.Length != 6 + 3 * body[5]) throw Invalid();
                Dimensions(BinaryPrimitives.ReadUInt16BigEndian(body[3..]), BinaryPrimitives.ReadUInt16BigEndian(body[1..])); frame = true;
            }
            else if (marker == 0xDA)
            {
                if (!frame || body.Length < 4 || body[0] is not (1 or 3) || body.Length != 4 + 2 * body[0]
                    || body[^3] != 0 || body[^2] != 63 || body[^1] != 0) throw Invalid();
                output.Write(bytes.Slice(start, pos - start));
                var scan = pos;
                while (pos < bytes.Length)
                {
                    if (bytes[pos++] != 255) continue;
                    while (pos < bytes.Length && bytes[pos] == 255) pos++;
                    if (pos >= bytes.Length) throw Invalid();
                    var next = bytes[pos++];
                    if (next == 0 || next is >= 0xD0 and <= 0xD7) continue;
                    if (next != 0xD9 || pos != bytes.Length) throw Invalid();
                    output.Write(bytes[scan..]); return output.ToArray();
                }
                throw Invalid();
            }
            else if (marker is >= 0xE0 and <= 0xEF || marker == 0xFE) continue; // profiles, EXIF, comments
            else if (marker is not (0xDB or 0xC4 or 0xDD)) throw Invalid();
            output.Write(bytes.Slice(start, pos - start));
        }
        throw Invalid();
    }
}
