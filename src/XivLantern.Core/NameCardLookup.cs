using System.Globalization;
using System.Net;
using System.Text.Json;

namespace XivLantern.Core;

/// <summary>NIP-05 domain mapping, not proof of a person's identity. The caller
/// must show the returned public key before a separate invite action.</summary>
public static class NameCardLookup
{
    public const int MaxResponseBytes = 65536;
    public static (string Name, Uri Endpoint) Address(string identifier)
    {
        ArgumentNullException.ThrowIfNull(identifier);
        if (identifier.Length > 320 || identifier != identifier.Trim()) throw new ArgumentException("Invalid name-card address.");
        var parts = identifier.Split('@');
        if (parts.Length != 2 || parts[0].Length is < 1 or > 64
            || parts[0].Any(c => c is not (>= 'a' and <= 'z' or >= '0' and <= '9' or '-' or '_' or '.')))
            throw new ArgumentException("Use name@domain with a lowercase name.");
        string domain;
        try { domain = new IdnMapping().GetAscii(parts[1]).ToLowerInvariant(); }
        catch (ArgumentException) { throw new ArgumentException("Invalid name-card domain."); }
        var labels = domain.Split('.');
        if (domain.Length > 253 || labels.Length < 2 || IPAddress.TryParse(domain, out _)
            || labels.Any(l => l.Length is < 1 or > 63 || l[0] == '-' || l[^1] == '-'
                || l.Any(c => c is not (>= 'a' and <= 'z' or >= '0' and <= '9' or '-'))))
            throw new ArgumentException("Invalid name-card domain.");
        return (parts[0], new Uri($"https://{domain}/.well-known/nostr.json?name={Uri.EscapeDataString(parts[0])}"));
    }

    public static string PublicKey(string name, ReadOnlyMemory<byte> json)
    {
        if (json.Length > MaxResponseBytes) throw new ArgumentException("Name-card response is too large.");
        using var document = JsonDocument.Parse(json, new JsonDocumentOptions { MaxDepth = 16 });
        if (document.RootElement.ValueKind != JsonValueKind.Object) throw new ArgumentException("Invalid name-card response.");
        var mappings = document.RootElement.EnumerateObject().Where(p => p.NameEquals("names")).ToArray();
        if (mappings.Length != 1 || mappings[0].Value.ValueKind != JsonValueKind.Object)
            throw new ArgumentException("Missing or ambiguous name-card mapping.");
        var matches = mappings[0].Value.EnumerateObject().Where(p => p.NameEquals(name)).ToArray();
        if (matches.Length != 1 || matches[0].Value.ValueKind != JsonValueKind.String)
            throw new ArgumentException("Name was not found unambiguously.");
        var key = matches[0].Value.GetString()!;
        if (key.Length != 64 || key.Any(c => c is not (>= '0' and <= '9' or >= 'a' and <= 'f')))
            throw new ArgumentException("Name-card mapping did not return a lowercase hex public key.");
        return key;
    }

    /// <summary>Use a client with AllowAutoRedirect=false. No relay suggestions
    /// from the domain are used. The caller explicitly initiates every lookup.</summary>
    public static async Task<string> Resolve(HttpClient client, string identifier, CancellationToken cancellation = default)
    {
        var (name, endpoint) = Address(identifier);
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        deadline.CancelAfter(TimeSpan.FromSeconds(10));
        using var request = new HttpRequestMessage(HttpMethod.Get, endpoint);
        using var response = await client.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, deadline.Token).ConfigureAwait(false);
        if (response.RequestMessage?.RequestUri != endpoint || (int)response.StatusCode is >= 300 and < 400)
            throw new ArgumentException("Name-card redirects are not allowed.");
        response.EnsureSuccessStatusCode();
        if (response.Content.Headers.ContentLength > MaxResponseBytes) throw new ArgumentException("Name-card response is too large.");
        using var input = await response.Content.ReadAsStreamAsync(deadline.Token).ConfigureAwait(false);
        using var body = new MemoryStream();
        var chunk = new byte[4096];
        while (true)
        {
            var read = await input.ReadAsync(chunk.AsMemory(0, Math.Min(chunk.Length, MaxResponseBytes + 1 - (int)body.Length)), deadline.Token).ConfigureAwait(false);
            if (read == 0) break;
            body.Write(chunk, 0, read);
            if (body.Length > MaxResponseBytes) throw new ArgumentException("Name-card response is too large.");
        }
        return PublicKey(name, body.ToArray());
    }
}
