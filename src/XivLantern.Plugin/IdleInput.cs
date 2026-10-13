using System.Runtime.InteropServices;

namespace XivLantern.Plugin;

internal static class IdleInput
{
    [StructLayout(LayoutKind.Sequential)]
    private struct LastInput { public uint Size; public uint Time; }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetLastInputInfo(ref LastInput input);

    [DllImport("kernel32.dll")]
    private static extern uint GetTickCount();

    public static long? ReadMilliseconds()
    {
        if (!OperatingSystem.IsWindows()) return null;
        try
        {
            var input = new LastInput { Size = (uint)Marshal.SizeOf<LastInput>() };
            if (!GetLastInputInfo(ref input)) return null;
            return XivLantern.Core.IdleAway.Elapsed(GetTickCount(), input.Time);
        }
        catch (DllNotFoundException) { return null; }
        catch (EntryPointNotFoundException) { return null; }
    }
}
