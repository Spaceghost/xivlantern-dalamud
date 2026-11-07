using System.Text.Json;
using XivLantern.Core;

namespace XivLantern.Core.Tests;

public class ActivityTagTests
{
    [Fact]
    public void PlayerTagsAreCleanBoundedUniqueAndPersistable()
    {
        var input = new[] { " Maps ", "maps", "\0craft\ning", "", new string('界', 30) }
            .Concat(Enumerable.Range(0, 30).Select(i => "tag" + i));
        var tags = ActivityTags.Normalize(input);
        Assert.Equal(8, tags.Count);
        Assert.Equal(new[] { "Maps", "crafting", new string('界', 10) }, tags.Take(3));
        Assert.All(tags, t => Assert.InRange(ChatText.Utf8Length(t), 1, 32));
        Assert.Equal(tags, ActivityTags.Normalize(JsonSerializer.Deserialize<List<string>>(JsonSerializer.Serialize(tags))));
        Assert.Empty(ActivityTags.Normalize(null));
    }

    [Fact]
    public void NativeFriendTagsAreOptionalAndUntrusted()
    {
        var current = Assert.Single(NativeJson.Friends("[{\"id\":\"abc\",\"online\":true,\"tags\":[\"maps\",12,null,\" MAPS \",\"chat\\u0000\"]}]"));
        Assert.Equal(new[] { "maps", "chat" }, current.Tags);
        Assert.Empty(Assert.Single(NativeJson.Friends("[{\"id\":\"abc\",\"online\":true}]")).Tags);
        Assert.Empty(Assert.Single(NativeJson.Friends("[{\"id\":\"abc\",\"online\":true,\"tags\":12}]")).Tags);
        Assert.Empty(Assert.Single(NativeJson.Friends("[{\"id\":\"abc\",\"online\":false,\"tags\":[\"maps\"]}]")).Tags);
    }

    [Fact]
    public void FilterMatchesOnlyOnlineActivitiesAndIpcHidesOfflineTags()
    {
        var friend = new FriendView(new string('a', 64), "Bob", true, "online", "", null, true, 1) { Tags = ["maps", "custom activity"] };
        Assert.True(ActivityTags.Matches(friend, "MAPS"));
        Assert.True(ActivityTags.Matches(friend, "custom activity"));
        Assert.False(ActivityTags.Matches(friend, "crafting"));
        Assert.True(ActivityTags.Matches(friend with { Online = false }, ""));
        Assert.False(ActivityTags.Matches(friend with { Online = false }, "maps"));
        using var online = JsonDocument.Parse(IpcPayloads.Friends([friend]));
        Assert.Equal(2, online.RootElement[0].GetProperty("tags").GetArrayLength());
        using var offline = JsonDocument.Parse(IpcPayloads.Friends([friend with { Online = false }]));
        Assert.Equal(0, offline.RootElement[0].GetProperty("tags").GetArrayLength());
    }
}
