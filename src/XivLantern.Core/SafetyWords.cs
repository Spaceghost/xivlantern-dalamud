using System.Numerics;
using System.Security.Cryptography;
using System.Text;

namespace XivLantern.Core;

public sealed record SafetyComparison(string Words, string Fingerprint);

public static class SafetyWords
{
    private const string ListHash = "addd35536511597a02fa0a9ff1e5284677b8883b83e986e43f15a3db996b903e";
    private static readonly Lazy<string[]> WordList = new(LoadWords);

    public static SafetyComparison? Compare(string? self, string? peer)
    {
        if (!ValidId(self) || !ValidId(peer)) return null;
        var a = Convert.FromHexString(self!);
        var b = Convert.FromHexString(peer!);
        var order = a.AsSpan().SequenceCompareTo(b);
        if (order == 0) return null;
        if (order > 0) (a, b) = (b, a);
        var domain = Encoding.ASCII.GetBytes("XivLantern safety words v1\0");
        var input = new byte[domain.Length + 64];
        domain.CopyTo(input, 0);
        a.CopyTo(input, domain.Length);
        b.CopyTo(input, domain.Length + 32);
        var hash = SHA256.HashData(input);
        var value = new BigInteger(hash, isUnsigned: true, isBigEndian: true);
        var words = WordList.Value;
        var result = new string[4];
        for (var i = 0; i < result.Length; i++)
        {
            result[i] = words[(int)(value % words.Length)];
            value /= words.Length;
        }
        return new(string.Join(' ', result), Convert.ToHexString(hash).ToLowerInvariant());
    }

    public static bool ValidId(string? id) => id is { Length: 64 } && id.All(Uri.IsHexDigit);

    public static bool IsVerified(IReadOnlyDictionary<string, string> marks, string self, string peer)
        => Compare(self, peer) is { } comparison && marks.TryGetValue(peer.ToLowerInvariant(), out var mark)
            && string.Equals(mark, comparison.Fingerprint, StringComparison.OrdinalIgnoreCase);

    private static string[] LoadWords()
    {
        using var stream = typeof(SafetyWords).Assembly.GetManifestResourceStream("XivLantern.Core.Resources.eff_large_wordlist.txt")
            ?? throw new InvalidOperationException("Safety word list is missing.");
        using var bytes = new MemoryStream();
        stream.CopyTo(bytes);
        var data = bytes.ToArray();
        if (!Convert.ToHexString(SHA256.HashData(data)).Equals(ListHash, StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException("Safety word list changed; a new comparison version is required.");
        var words = Encoding.UTF8.GetString(data).Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .Select(line => line.TrimEnd('\r').Split('\t')[1]).ToArray();
        if (words.Length != 7776 || words.Distinct(StringComparer.Ordinal).Count() != 7776)
            throw new InvalidOperationException("Safety word list is invalid.");
        return words;
    }
}
