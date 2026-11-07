using System.Numerics;
using Dalamud.Bindings.ImGui;
using Dalamud.Interface.Textures.TextureWraps;
using Dalamud.Plugin.Services;
using XivLantern.Core;

namespace XivLantern.Plugin;

/// <summary>One explicitly requested local preview at a time. File IO and decoder
/// work never run in Draw. All owned textures, including late results, are disposed.</summary>
internal sealed class FilePreview(ITextureProvider textures) : IDisposable
{
    private readonly object gate = new();
    private readonly CancellationTokenSource cancel = new();
    private IDalamudTextureWrap? texture;
    private ulong handle;
    private string destination = "";
    private bool busy, disposed, large;
    private string message = "";

    public void Draw(FileOffer offer)
    {
        lock (gate)
        {
            ImGui.BeginDisabled(busy || disposed);
            if (ImGui.SmallButton("Preview image"))
            {
                texture?.Dispose(); texture = null;
                handle = offer.Handle; destination = offer.Destination; large = false; busy = true; message = "Preparing preview…";
                _ = Load(offer.Destination);
            }
            ImGui.EndDisabled();
            if (handle != offer.Handle || destination != offer.Destination) return;
            if (message.Length > 0) ImGui.TextDisabled(message);
            if (texture is null) return;
            var limit = large ? new Vector2(640, 480) : new Vector2(200, 150);
            var scale = Math.Min(1f, Math.Min(limit.X / texture.Width, limit.Y / texture.Height));
            ImGui.Image(texture.Handle, new Vector2(texture.Width, texture.Height) * scale);
            if (ImGui.SmallButton(large ? "Show thumbnail" : "Open larger preview")) large = !large;
            ImGui.SameLine();
            if (ImGui.SmallButton("Close preview")) { texture.Dispose(); texture = null; message = ""; }
        }
    }

    private async Task Load(string path)
    {
        IDalamudTextureWrap? loaded = null;
        try
        {
            // Decode the admitted immutable snapshot, never reopen a mutable path
            // in the decoder. The native fetch already authenticated saved bytes.
            var bytes = await Task.Run(() => ImagePreview.Admit(FileDownloads.ReadOffer(path).Bytes), cancel.Token);
            loaded = await textures.CreateFromImageAsync(bytes, "Lantern file preview", cancel.Token);
            if (loaded.Width <= 0 || loaded.Height <= 0 || loaded.Width > ImagePreview.MaxSide || loaded.Height > ImagePreview.MaxSide
                || (long)loaded.Width * loaded.Height > ImagePreview.MaxPixels) throw new IOException("Decoded image exceeds preview limits.");
            lock (gate)
            {
                if (!disposed) { texture = loaded; loaded = null; message = ""; }
            }
        }
        catch (OperationCanceledException) { }
        catch (Exception) { lock (gate) if (!disposed) message = "Preview unavailable. Use a static PNG or baseline JPEG within the image limits."; }
        finally { loaded?.Dispose(); lock (gate) { busy = false; if (disposed) cancel.Dispose(); } }
    }
    public void Dispose()
    {
        lock (gate)
        {
            if (disposed) return;
            disposed = true; cancel.Cancel(); texture?.Dispose(); texture = null;
            if (!busy) cancel.Dispose();
        }
        // A live worker disposes its token source after the decoder completes.
    }
}
