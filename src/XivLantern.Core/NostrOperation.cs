namespace XivLantern.Core;

/// <summary>One manual operation at a time. Native completion may arrive before
/// the worker posts its returned handle; bounded early results cover that ordering.</summary>
public sealed class NostrOperation
{
    private bool preparing;
    private ulong job;
    private readonly Dictionary<ulong, string> early = [];
    public bool Busy => preparing || job != 0;
    public string Status { get; private set; } = "";

    public bool Begin()
    {
        if (Busy) return false;
        preparing = true;
        early.Clear();
        Status = "Preparing…";
        return true;
    }

    public void Prepared(string status, ulong handle = 0)
    {
        if (!preparing) return;
        preparing = false;
        Status = status;
        job = handle;
        if (handle != 0 && early.TryGetValue(handle, out var completed))
        {
            job = 0;
            Status = completed;
        }
        early.Clear();
    }

    public void Completed(ulong handle, string status)
    {
        if (handle == 0) return;
        if (job == handle)
        {
            job = 0;
            Status = status;
        }
        else if (preparing && early.Count < 8) early[handle] = status;
    }

    public void Reset()
    {
        preparing = false;
        job = 0;
        early.Clear();
        Status = "";
    }
}
