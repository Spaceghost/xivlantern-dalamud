using System.Buffers.Binary;
using System.IO.Compression;

namespace XivLantern.Core.Tests;

public class ImagePreviewTests
{
    private static byte[] Fixture(string name) => File.ReadAllBytes(Path.Combine(AppContext.BaseDirectory, "Fixtures", name));
    [Theory]
    [InlineData("preview.png")]
    [InlineData("preview.jpg")]
    public void RealSmallImagesAreAdmittedWithPrivateMetadataRemoved(string name)
    {
        var bytes = Fixture(name);
        var admitted = ImagePreview.Admit(bytes);
        Assert.True(admitted.Length < bytes.Length);
        Assert.DoesNotContain("private metadata", System.Text.Encoding.Latin1.GetString(admitted));
        Assert.Equal(admitted, ImagePreview.Admit(admitted));
    }
    [Fact]
    public void RejectsProgressiveTruncatedUnknownAndOversizedEncodedInput()
    {
        Assert.Throws<IOException>(() => ImagePreview.Admit(Fixture("preview-progressive.jpg")));
        foreach (var name in new[] { "preview.png", "preview.jpg" })
        {
            var bytes = Fixture(name);
            Assert.Throws<IOException>(() => ImagePreview.Admit(bytes[..^1]));
            Assert.Throws<IOException>(() => ImagePreview.Admit(bytes.Concat(new byte[] { 0 }).ToArray()));
        }
        Assert.Throws<IOException>(() => ImagePreview.Admit("not an image"u8));
        Assert.Throws<IOException>(() => ImagePreview.Admit(new byte[(int)FileOffers.MaxBytes + 1]));
    }
    [Fact]
    public void BothFormatsRejectDimensionAndPixelBudgetViolationsBeforeDecoding()
    {
        foreach (var (width,height) in new[] { (0,1), (4097,1), (4096,4096) })
        {
            var png = Fixture("preview.png");
            BinaryPrimitives.WriteUInt32BigEndian(png.AsSpan(16), (uint)width);
            BinaryPrimitives.WriteUInt32BigEndian(png.AsSpan(20), (uint)height);
            Assert.Throws<IOException>(() => ImagePreview.Admit(png));
            var jpeg = Fixture("preview.jpg");
            var frame = -1;
            for (var i = 0; i < jpeg.Length - 1; i++) if (jpeg[i] == 255 && jpeg[i+1] == 0xC0) { frame = i; break; }
            Assert.True(frame >= 0);
            BinaryPrimitives.WriteUInt16BigEndian(jpeg.AsSpan(frame+5), (ushort)height);
            BinaryPrimitives.WriteUInt16BigEndian(jpeg.AsSpan(frame+7), (ushort)width);
            Assert.Throws<IOException>(() => ImagePreview.Admit(jpeg));
        }
    }
    [Fact]
    public void PngRejectsAnimationAndCompressedPayloadBeyondClaimedPixels()
    {
        var png = Fixture("preview.png");
        var animation = new byte[12]; "acTL"u8.CopyTo(animation.AsSpan(4));
        Assert.Throws<IOException>(() => ImagePreview.Admit(png[..33].Concat(animation).Concat(png[33..]).ToArray()));
        using var compressed = new MemoryStream();
        using (var z = new ZLibStream(compressed, CompressionLevel.SmallestSize, true)) z.Write(new byte[1024 * 1024]);
        var payload = compressed.ToArray();
        var chunk = new byte[payload.Length + 12];
        BinaryPrimitives.WriteUInt32BigEndian(chunk, (uint)payload.Length); "IDAT"u8.CopyTo(chunk.AsSpan(4)); payload.CopyTo(chunk,8);
        var forged = png[..33].Concat(chunk).Concat(png[^12..]).ToArray();
        Assert.Throws<IOException>(() => ImagePreview.Admit(forged));
    }
}
