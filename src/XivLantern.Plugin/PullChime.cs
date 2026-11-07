using System.Runtime.InteropServices;

namespace XivLantern.Plugin;

internal static class PullChime
{
    [DllImport("winmm.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PlaySoundW(string sound, nint module, uint flags);

    public static void Play()
    {
        if (!OperatingSystem.IsWindows()) return;
        try
        {
            // Async system notification sound; yield to another sound and stay silent if unavailable.
            PlaySoundW("SystemAsterisk", 0, 0x10000 | 0x1 | 0x2 | 0x10 | 0x200000);
        }
        catch (DllNotFoundException) { }
        catch (EntryPointNotFoundException) { }
    }
}
