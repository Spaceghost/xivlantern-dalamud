using XivLantern.Core;

namespace XivLantern.Core.Tests;

public class SafetyWordTests
{
    private static readonly string Alice = new('0', 64);
    private static readonly string Bob = new('f', 64);

    [Fact]
    public void BothPeersHaveSamePinnedVersionedComparison()
    {
        var comparison = SafetyWords.Compare(Alice, Bob)!;
        Assert.Equal(comparison, SafetyWords.Compare(Bob.ToUpperInvariant(), Alice));
        Assert.Equal("pampered opposing trash unlawful", comparison.Words);
        Assert.Equal("869c10b26cbd183a52687f82282c43db58ee8a959ea6540c3fb15b3781a34abe", comparison.Fingerprint);
        Assert.Equal(4, comparison.Words.Split(' ').Length);
    }

    [Fact]
    public void EitherIdentityChangingInvalidatesLocalComparisonMark()
    {
        var comparison = SafetyWords.Compare(Alice, Bob)!;
        var marks = new Dictionary<string, string> { [Bob] = comparison.Fingerprint };
        Assert.True(SafetyWords.IsVerified(marks, Alice, Bob.ToUpperInvariant()));
        Assert.False(SafetyWords.IsVerified(marks, new string('1', 64), Bob));
        Assert.False(SafetyWords.IsVerified(marks, Alice, new string('e', 64)));
        Assert.NotEqual(comparison.Words, SafetyWords.Compare(Alice, new string('e', 64))!.Words);
        Assert.False(SafetyWords.IsVerified(new Dictionary<string, string>(), Alice, Bob));
        marks[Bob] = new string('0', 64);
        Assert.False(SafetyWords.IsVerified(marks, Alice, Bob));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("not a node")]
    [InlineData("000000000000000000000000000000000000000000000000000000000000000g")]
    public void InvalidAndMissingIdentitiesNeverProduceComparison(string? bad)
    {
        Assert.Null(SafetyWords.Compare(Alice, bad));
        Assert.Null(SafetyWords.Compare(bad, Bob));
        Assert.Null(SafetyWords.Compare(Alice, Alice));
    }
}
