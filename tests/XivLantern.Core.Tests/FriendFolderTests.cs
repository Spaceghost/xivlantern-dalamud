using System.Text.Json;
using XivLantern.Core;

namespace XivLantern.Core.Tests;

public class FriendFolderTests
{
    private static FriendView Friend(char id, bool online) => new(new string(id, 64), "Same display name", online, "online", "", null, false, 0);

    [Fact]
    public void GroupsUseNodeIdentityAndCountOnlyCurrentFriends()
    {
        var a = Friend('a', true);
        var b = Friend('b', false);
        var c = Friend('c', true);
        var assignment = FriendFolders.Normalize(new Dictionary<string, string>
        {
            [a.Id] = "Static", [b.Id] = "static", [new string('d', 64)] = "Old server",
        });
        var groups = FriendFolders.Group([a, b, c], assignment);
        Assert.Collection(groups,
            folder => { Assert.Equal("Static", folder.Name); Assert.Equal(2, folder.Friends.Count); Assert.Equal(1, folder.Online); },
            folder => { Assert.Equal("", folder.Name); Assert.Equal(c, Assert.Single(folder.Friends)); });
        Assert.Empty(FriendFolders.Group([], assignment));
        Assert.Equal(0, FriendFolders.Group([a with { Online = false }, b], assignment)[0].Online);
    }

    [Fact]
    public void MovingAndUnassigningPersistWithoutChangingAnotherFriend()
    {
        var a = Friend('a', true);
        var b = Friend('b', true);
        var map = FriendFolders.Assign(new Dictionary<string, string>(), a.Id, " Static ", out var error);
        Assert.Empty(error);
        map = FriendFolders.Assign(map, b.Id, "FC", out error);
        map = FriendFolders.Assign(map, a.Id.ToUpperInvariant(), "Crafting", out error);
        var restored = FriendFolders.Normalize(JsonSerializer.Deserialize<Dictionary<string, string>>(JsonSerializer.Serialize(map)));
        Assert.Equal("Crafting", restored[a.Id]);
        Assert.Equal("FC", restored[b.Id]);
        restored = FriendFolders.Assign(restored, a.Id, "", out error);
        Assert.Empty(error);
        Assert.False(restored.ContainsKey(a.Id));
        Assert.Equal("FC", restored[b.Id]);
    }

    [Fact]
    public void FolderMutePreservesUnrelatedAndChannelKeys()
    {
        var a = Friend('a', true);
        var b = Friend('b', false);
        var c = Friend('c', true);
        var original = new HashSet<string> { ChatBook.FriendKey(c.Id), "c:raid", ChatBook.FriendKey(a.Id) };
        var folder = new FriendFolder("Static", [a, b]);
        var muted = FriendFolders.SetMuted(original, folder, true);
        Assert.Equal(4, muted.Count);
        Assert.Equal(3, original.Count);
        var unmuted = FriendFolders.SetMuted(muted, folder, false);
        Assert.Equal(new HashSet<string> { ChatBook.FriendKey(c.Id), "c:raid" }, unmuted);
    }

    [Fact]
    public void InvalidLoadedDataAndNamesAreRejected()
    {
        var a = Friend('a', true);
        Assert.Empty(FriendFolders.Normalize(null));
        Assert.Empty(FriendFolders.Normalize(new Dictionary<string, string> { [a.Id] = null!, ["invalid"] = "FC" }));
        foreach (var name in new[] { "bad\nname", "bad\0name", new string('界', 22) })
        {
            Assert.Empty(FriendFolders.Assign(new Dictionary<string, string>(), a.Id, name, out var error));
            Assert.NotEmpty(error);
        }
        Assert.Empty(FriendFolders.Assign(new Dictionary<string, string>(), "bad id", "FC", out var invalid));
        Assert.NotEmpty(invalid);
        Assert.Single(FriendFolders.Assign(new Dictionary<string, string>(), a.Id, new string('界', 21), out var valid));
        Assert.Empty(valid);
    }
}
