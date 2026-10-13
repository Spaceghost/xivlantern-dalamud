namespace XivLantern.Core;

/// <summary>Automatic presence is transient; the saved manual presence and note are never rewritten.</summary>
public sealed class IdleAway
{
    public static long? Elapsed(uint now, uint lastInput)
    {
        var elapsed = unchecked(now - lastInput);
        // More than half the clock range is ambiguous with a future/nonmonotonic input timestamp.
        return elapsed <= int.MaxValue ? elapsed : null;
    }
    public bool Active { get; private set; }
    private bool awaitActivity;

    public void ManualChanged()
    {
        Active = false;
        awaitActivity = true;
    }

    public void Tick(bool enabled, bool running, uint presence, string note, long? idleMilliseconds,
        long thresholdMilliseconds, Func<uint, string, bool> publish)
    {
        if (!running) { Active = false; awaitActivity = false; return; }
        var idle = idleMilliseconds is >= 0 ? idleMilliseconds : null;
        var threshold = Math.Max(1, thresholdMilliseconds);
        if (idle < threshold) awaitActivity = false;
        if (Active)
        {
            // Manual busy, invisible and away always take precedence, including external config edits.
            if (!enabled || presence != 1 || idle == null || idle < threshold)
                if (publish(presence, note)) Active = false;
            return;
        }
        if (enabled && presence == 1 && !awaitActivity && idle >= threshold)
            if (publish(2, note)) Active = true;
    }
}
