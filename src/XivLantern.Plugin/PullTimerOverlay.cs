using Dalamud.Bindings.ImGui;
using System.Numerics;
using XivLantern.Core;

namespace XivLantern.Plugin;

internal static class PullTimerOverlay
{
    public static void Draw(PullCountdown timer)
    {
        if (timer.Remaining(Environment.TickCount64) is not { } seconds) return;
        var display = ImGui.GetIO().DisplaySize;
        ImGui.SetNextWindowPos(new Vector2(display.X / 2, display.Y / 3), ImGuiCond.Always, new Vector2(0.5f));
        ImGui.SetNextWindowBgAlpha(0.75f);
        if (ImGui.Begin("Lantern pull###XivLanternPull", ImGuiWindowFlags.NoInputs | ImGuiWindowFlags.NoDecoration
            | ImGuiWindowFlags.AlwaysAutoResize | ImGuiWindowFlags.NoSavedSettings | ImGuiWindowFlags.NoFocusOnAppearing))
        {
            ImGui.TextUnformatted("#" + ChatText.Preview(timer.Label, 40));
            ImGui.SetWindowFontScale(2.5f);
            ImGui.TextUnformatted(seconds == 0 ? "Go" : seconds.ToString(System.Globalization.CultureInfo.InvariantCulture));
            ImGui.SetWindowFontScale(1);
        }
        ImGui.End();
    }
}
