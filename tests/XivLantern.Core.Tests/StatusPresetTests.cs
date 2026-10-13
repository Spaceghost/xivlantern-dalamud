using System.Text.Json;
using XivLantern.Core;

namespace XivLantern.Core.Tests;

public class StatusPresetTests
{
    [Theory]
    [InlineData("as raid", "raid")]
    [InlineData("AS Raid Night", "Raid Night")]
    [InlineData(" as   on ", "on")]
    [InlineData("as", "")]
    public void PresetVerbPreservesWholeNameWithoutCollidingWithCommands(string text, string name)
        => Assert.Equal(new Command(Verb.As, name), Command.Parse(text));

    [Fact]
    public void SaveReplaceResolveRemoveAndSerializationRetainPresenceAndNote()
    {
        var presets = StatusPresets.Save([], " raid ", 3, "prog, back at 23:00", out var error);
        Assert.Equal("", error);
        var preset = Assert.Single(presets);
        Assert.Equal(("raid", 3u, "prog, back at 23:00"), (preset.Name, preset.Presence, preset.Note));
        presets = StatusPresets.Save(presets, "RAID", 2, "brb", out error);
        Assert.Single(presets);
        Assert.Equal(2u, StatusPresets.Find(presets, " raid ")!.Presence);
        var restored = JsonSerializer.Deserialize<List<StatusPreset>>(JsonSerializer.Serialize(presets))!;
        Assert.Equal(presets, restored);
        Assert.Empty(StatusPresets.Remove(restored, "raid"));
        Assert.Null(StatusPresets.Find(restored, "missing"));
        Assert.Equal(Verb.On, Command.Parse("on").Verb);
    }

    [Fact]
    public void NotesUseExistingUtf8LimitAndNamesCannotContainControlCharacters()
    {
        var presets = StatusPresets.Save([], "chill", 1, "\0" + new string('界', 400), out var error);
        Assert.Equal("", error);
        var note = Assert.Single(presets).Note;
        Assert.DoesNotContain('\0', note);
        Assert.True(ChatText.Utf8Length(note) <= ChatText.MaxNote * 4);
        Assert.EndsWith("界", note);
        foreach (var name in new[] { "", "bad\nname", "bad\tname", new string('界', 22) })
        {
            Assert.Empty(StatusPresets.Save([], name, 1, "", out error));
            Assert.NotEmpty(error);
        }
        Assert.Empty(StatusPresets.Save([], "invalid", 4, "", out error));
        Assert.NotEmpty(error);
    }

    [Fact]
    public void LoadedConfigurationDropsInvalidDuplicateEntriesAndBoundsList()
    {
        var presets = new List<StatusPreset>
        {
            null!, new() { Name = "" }, new() { Name = "bad", Presence = 99 },
            new() { Name = " Raid ", Presence = 3, Note = "prog" }, new() { Name = "raid", Note = "duplicate" },
        };
        presets.AddRange(Enumerable.Range(0, 40).Select(i => new StatusPreset { Name = $"Preset {i}" }));
        var normalized = StatusPresets.Normalize(presets);
        Assert.Equal(StatusPresets.MaxPresets, normalized.Count);
        Assert.Equal("prog", StatusPresets.Find(normalized, "raid")!.Note);
        Assert.Empty(StatusPresets.Normalize(null));
        Assert.Equal(normalized, StatusPresets.Save(normalized, "overflow", 1, "", out var error));
        Assert.NotEmpty(error);
        var replaced = StatusPresets.Save(normalized, "RAID", 1, "updated", out error);
        Assert.Empty(error);
        Assert.Equal(StatusPresets.MaxPresets, replaced.Count);
        Assert.Equal("updated", StatusPresets.Find(replaced, "raid")!.Note);
    }
}
