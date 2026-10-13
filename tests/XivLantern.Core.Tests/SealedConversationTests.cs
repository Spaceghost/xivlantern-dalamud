using System.Text.Json;
using XivLantern.Core;

namespace XivLantern.Core.Tests;

public class SealedConversationTests
{
    private const string Ticket = "ltsealedafykia7hgviuabcdefghijk";

    [Fact]
    public void SealedConversationIdsAreOpaqueStableAndCaseNormalized()
    {
        var key = ChatBook.ChannelKey(Ticket);
        Assert.StartsWith("c:sealed:", key);
        Assert.Equal(73, key.Length);
        Assert.DoesNotContain(Ticket, key);
        Assert.Equal(key, ChatBook.ChannelKey(" \t" + Ticket.ToUpperInvariant() + "\n"));
        Assert.NotEqual(key, ChatBook.ChannelKey(Ticket + "a"));
        Assert.Equal("c:ltroomabcdefghijk", ChatBook.ChannelKey("ltroomabcdefghijk"));
        var book = new ChatBook();
        Assert.Same(book.Get(key, "Private"), book.Get(ChatBook.ChannelKey(Ticket), "Private"));
        Assert.DoesNotContain(Ticket, JsonSerializer.Serialize(book.All));
    }

    [Fact]
    public void LegacySealedMutesMigrateWithoutChangingOtherConversations()
    {
        var friend = ChatBook.FriendKey(new string('a', 64));
        var original = new HashSet<string> { "c:" + Ticket, "c:" + Ticket.ToUpperInvariant(),
            friend, "c:ltroomabcdefghijk", ChatBook.AuthorKey, ChatBook.SupportKey };
        var migrated = ChatBook.NormalizeConversationKeys(original);
        Assert.Equal(5, migrated.Count);
        Assert.Contains(ChatBook.ChannelKey(Ticket), migrated);
        var settings = new NotifySettings(true, true, true, true, migrated);
        Assert.False(NotifyPolicy.CountsUnread(ChatBook.ChannelKey(Ticket), settings, null));
        Assert.False(NotifyPolicy.Toast(Incoming.ChannelLine, ChatBook.ChannelKey(Ticket), settings, null));
        Assert.Contains(friend, migrated);
        Assert.Contains("c:ltroomabcdefghijk", migrated);
        Assert.Contains(ChatBook.AuthorKey, migrated);
        Assert.Contains(ChatBook.SupportKey, migrated);
        Assert.Contains("c:" + Ticket, original); // No mutation of caller data.
        Assert.DoesNotContain(Ticket, JsonSerializer.Serialize(migrated));
        Assert.Equal(migrated, ChatBook.NormalizeConversationKeys(migrated));
        Assert.Empty(ChatBook.NormalizeConversationKeys(null));
    }
}
