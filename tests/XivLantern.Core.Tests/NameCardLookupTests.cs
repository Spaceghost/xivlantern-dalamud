using System.Net;
using System.Text;

namespace XivLantern.Core.Tests;

public class NameCardLookupTests
{
    private const string Key = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    [Fact]
    public void NamesUseExactHttpsWellKnownQueryAndRejectUrlInjection()
    {
        Assert.Equal("https://example.org/.well-known/nostr.json?name=cid", NameCardLookup.Address("cid@EXAMPLE.org").Endpoint.AbsoluteUri);
        foreach (var invalid in new[] { "cid", "a@b@c.org", "a@127.0.0.1", "a@localhost", "a@example.org:443", "a@example.org/path", "a@example.org?x=y", "a@example.org#x", " a@example.org", "Alice@example.org", "a+z@example.org", "a@-bad.org" })
            Assert.Throws<ArgumentException>(() => NameCardLookup.Address(invalid));
    }
    [Fact]
    public void MappingRejectsDuplicatesWrongKeysAndOversizedJson()
    {
        byte[] Json(string s) => Encoding.UTF8.GetBytes(s);
        Assert.Equal(Key, NameCardLookup.PublicKey("cid", Json($"{{\"names\":{{\"cid\":\"{Key}\"}},\"relays\":{{}}}}")));
        foreach (var json in new[] { "[]", "{}", "{\"names\":[]}", $"{{\"names\":{{\"cid\":\"{Key}\",\"cid\":\"{Key}\"}}}}", $"{{\"names\":{{\"cid\":\"{Key}\"}},\"names\":{{}}}}", $"{{\"names\":{{\"cid\":\"{Key.ToUpperInvariant()}\"}}}}" })
            Assert.Throws<ArgumentException>(() => NameCardLookup.PublicKey("cid", Json(json)));
        Assert.Throws<ArgumentException>(() => NameCardLookup.PublicKey("cid", new byte[65537]));
    }
    private sealed class Fake(Func<HttpRequestMessage, HttpResponseMessage> response) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var result = response(request); result.RequestMessage ??= request; return Task.FromResult(result);
        }
    }
    [Fact]
    public async Task LookupUsesOnlyChosenDomainAndRejectsRedirectsAndLargeResponses()
    {
        using var client = new HttpClient(new Fake(request =>
        {
            Assert.Equal(HttpMethod.Get, request.Method);
            Assert.Equal("https://example.org/.well-known/nostr.json?name=cid", request.RequestUri!.AbsoluteUri);
            return new(HttpStatusCode.OK) { Content = new StringContent($"{{\"names\":{{\"cid\":\"{Key}\"}}}}") };
        }));
        Assert.Equal(Key, await NameCardLookup.Resolve(client, "cid@example.org"));
        using var redirect = new HttpClient(new Fake(_ => new(HttpStatusCode.Redirect)));
        await Assert.ThrowsAsync<ArgumentException>(() => NameCardLookup.Resolve(redirect, "cid@example.org"));
        using var oversized = new HttpClient(new Fake(_ => new(HttpStatusCode.OK) { Content = new ByteArrayContent(new byte[65537]) }));
        await Assert.ThrowsAsync<ArgumentException>(() => NameCardLookup.Resolve(oversized, "cid@example.org"));
    }
    private sealed class UnknownLengthStream(byte[] bytes) : MemoryStream(bytes)
    {
        public override bool CanSeek => false;
        public int BytesRead { get; private set; }
        public bool Closed { get; private set; }
        public override async ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellation = default)
        {
            var read = await base.ReadAsync(buffer, cancellation);
            BytesRead += read;
            return read;
        }
        protected override void Dispose(bool disposing) { Closed = true; base.Dispose(disposing); }
    }

    [Fact]
    public async Task UnknownLengthResponseStopsAtLimitAndDisposesTheConnection()
    {
        var input = new UnknownLengthStream(new byte[1024 * 1024]);
        using var client = new HttpClient(new Fake(_ =>
        {
            var content = new StreamContent(input);
            Assert.Null(content.Headers.ContentLength);
            return new(HttpStatusCode.OK) { Content = content };
        }));
        await Assert.ThrowsAsync<ArgumentException>(() => NameCardLookup.Resolve(client, "cid@example.org"));
        Assert.Equal(NameCardLookup.MaxResponseBytes + 1, input.BytesRead);
        Assert.True(input.Closed);
    }

    private sealed class Stalled : HttpMessageHandler
    {
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            await Task.Delay(Timeout.InfiniteTimeSpan, cancellationToken);
            throw new InvalidOperationException("Cancellation must interrupt the lookup.");
        }
    }

    [Fact]
    public async Task CallerCancellationInterruptsLookupWithoutWaitingForDefaultDeadline()
    {
        using var client = new HttpClient(new Stalled());
        using var cancellation = new CancellationTokenSource();
        var lookup = NameCardLookup.Resolve(client, "cid@example.org", cancellation.Token);
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(async () => await lookup.WaitAsync(TimeSpan.FromSeconds(2)));
    }

}
