namespace XivLantern.Core;

public sealed record StatusPreset
{
    public string Name { get; init; } = "";
    public uint Presence { get; init; } = 1;
    public string Note { get; init; } = "";
}

public static class StatusPresets
{
    public const int MaxPresets = 32;
    public const int MaxNameBytes = 64;

    public static StatusPreset? Find(IReadOnlyList<StatusPreset> presets, string name)
        => presets.FirstOrDefault(p => string.Equals(p.Name, name.Trim(), StringComparison.OrdinalIgnoreCase));

    public static List<StatusPreset> Normalize(IEnumerable<StatusPreset>? presets)
    {
        var result = new List<StatusPreset>();
        foreach (var preset in presets ?? [])
        {
            if (preset == null) continue;
            var normalized = Create(preset.Name, preset.Presence, preset.Note, out _);
            if (normalized == null || Find(result, normalized.Name) != null) continue;
            result.Add(normalized);
            if (result.Count == MaxPresets) break;
        }
        return result;
    }

    public static List<StatusPreset> Save(IReadOnlyList<StatusPreset> presets, string name, uint presence, string note, out string error)
    {
        var result = Normalize(presets);
        var preset = Create(name, presence, note, out error);
        if (preset == null) return result;
        var existing = Find(result, preset.Name);
        if (existing != null) result[result.IndexOf(existing)] = preset;
        else if (result.Count < MaxPresets) result.Add(preset);
        else error = $"You can save up to {MaxPresets} status presets.";
        return result;
    }

    public static List<StatusPreset> Remove(IReadOnlyList<StatusPreset> presets, string name)
        => presets.Where(p => !string.Equals(p.Name, name.Trim(), StringComparison.OrdinalIgnoreCase)).ToList();

    private static StatusPreset? Create(string? name, uint presence, string? note, out string error)
    {
        name = (name ?? "").Trim();
        error = "";
        if (name.Length == 0 || ChatText.Utf8Length(name) > MaxNameBytes || name.Any(char.IsControl))
        {
            error = $"Choose a preset name of 1–{MaxNameBytes} UTF-8 bytes without control characters.";
            return null;
        }
        if (presence > 3) { error = "Invalid presence; choose online, away, busy or invisible."; return null; }
        return new StatusPreset { Name = name, Presence = presence,
            Note = ChatText.Clamp(ChatText.Clean(note ?? ""), ChatText.MaxNote * 4) };
    }
}
