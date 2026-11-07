/* Synthetic private peers in the disposable Wine prefix; no public relays.
 * Exercise the actual Windows DLL's friend-file network and filesystem path. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>
#include "lantern.h"

static lt_node *a, *b;
static uint8_t aid[32], bid[32];
static unsigned aport, bport;
static const char *failure;
static lt_room expected_offer_room;
static uint8_t last_record[2210];
static uint32_t last_record_length;
#define CHECK(x, why) do { if (!(x)) { failure = (why); goto cleanup; } } while (0)

static int wait_event(lt_node *target, uint32_t kind, uint64_t handle, lt_blob *offer) {
    ULONGLONG deadline = GetTickCount64() + 30000;
    lt_event events[32];
    while (GetTickCount64() < deadline) {
        lt_node *nodes[] = {a, b};
        for (int n = 0; n < 2; n++) {
            int count = lt_poll(nodes[n], events, 32);
            if (count < 0) return 0;
            for (int i = 0; i < count; i++) {
                if (nodes[n] != target) continue;
                if (events[i].kind == LT_EV_BLOB_FAILED && events[i].handle == handle && kind == LT_EV_BLOB_DONE) {
                    fprintf(stderr, "Windows fetch failed: %.*s\n", (int)events[i].data_len, events[i].data);
                    return 0;
                }
                if (events[i].kind != kind || (handle && events[i].handle != handle)) continue;
                if (kind == LT_EV_SELFTEST) {
                    char report[4097];
                    if (!events[i].data || events[i].data_len >= sizeof report) return 0;
                    memcpy(report, events[i].data, events[i].data_len); report[events[i].data_len] = 0;
                    char *bound = strstr(report, "0.0.0.0:");
                    if (!bound || !strstr(report, "\"direct\":{\"ok\":true")) return 0;
                    unsigned port = (unsigned)strtoul(bound + 8, NULL, 10);
                    if (port == 0 || port > 65535) return 0;
                    if (target == a) aport = port; else bport = port;
                }
                if (kind == LT_EV_BLOB_OFFER) {
                    if (memcmp(events[i].peer, aid, sizeof aid) || !events[i].data) return 0;
                    /* Copy before another poll reuses native event bytes. */
                    char metadata[4097];
                    if (events[i].data_len >= sizeof metadata) return 0;
                    memcpy(metadata, events[i].data, events[i].data_len); metadata[events[i].data_len] = 0;
                    char room_field[64];
                    snprintf(room_field, sizeof room_field, "\"room\":%llu,", (unsigned long long)expected_offer_room);
                    if (!strstr(metadata, room_field) || !strstr(metadata, "clear.png")) return 0;
                    *offer = events[i].handle;
                }
                if (kind == LT_EV_CHANNEL_RECORD || kind == LT_EV_CHANNEL_HISTORY) {
                    if (!events[i].data || events[i].data_len > sizeof last_record) return 0;
                    memcpy(last_record, events[i].data, events[i].data_len);
                    last_record_length = events[i].data_len;
                }
                return 1;
            }
        }
        Sleep(10);
    }
    return 0;
}
static int terminal(lt_blob blob) {
    ULONGLONG deadline = GetTickCount64() + 30000;
    lt_event events[32];
    while (GetTickCount64() < deadline) {
        int count = lt_poll(b, events, 32);
        if (count < 0) return 0;
        for (int i = 0; i < count; i++) {
            if (events[i].handle != blob) continue;
            if (events[i].kind == LT_EV_BLOB_DONE) return 1;
            if (events[i].kind == LT_EV_BLOB_FAILED) return 2;
        }
        lt_poll(a, events, 32); Sleep(10);
    }
    return 0;
}
static int exact_file(const char *path, const uint8_t *bytes, size_t size) {
    FILE *file = fopen(path, "rb");
    if (!file) return 0;
    uint8_t block[4096]; size_t offset = 0; int ok = 1;
    while (offset < size) {
        size_t expected = size - offset < sizeof block ? size - offset : sizeof block;
        if (fread(block, 1, expected, file) != expected || memcmp(block, bytes + offset, expected)) { ok = 0; break; }
        offset += expected;
    }
    if (ok && fgetc(file) != EOF) ok = 0;
    if (ferror(file)) ok = 0;
    fclose(file); return ok;
}
static int local_hint(const char *label, const uint8_t *id, unsigned port) {
    char path[64], temporary[64]; snprintf(path, sizeof path, "%s-hint-request.txt", label);
    snprintf(temporary, sizeof temporary, "%s-hint-request.tmp", label);
    FILE *file = fopen(temporary, "wb"); if (!file) return 0;
    for (int i = 0; i < 32; i++) fprintf(file, "%02x", id[i]);
    fprintf(file, "\n%u\n", port);
    return fclose(file) == 0 && rename(temporary, path) == 0;
}
static int receive_hint(lt_node *target, const char *label) {
    char path[64], ticket[LT_MAX_TEXT]; snprintf(path, sizeof path, "%s-hint.txt", label);
    ULONGLONG deadline = GetTickCount64() + 30000;
    while (GetTickCount64() < deadline) {
        FILE *file = fopen(path, "rb");
        if (file) {
            size_t n = fread(ticket, 1, sizeof ticket - 1, file); int ok = !ferror(file) && feof(file);
            fclose(file); ticket[n] = 0;
            return ok && lt_address_hint(target, ticket) == LT_OK;
        }
        Sleep(10);
    }
    return 0;
}
static int sealed_transfer(const uint8_t *bytes, size_t size) {
    typedef lt_status (*create_fn)(lt_node *, const char *, lt_room *);
    typedef lt_status (*reseal_fn)(lt_node *, lt_room, lt_room *);
    typedef lt_status (*add_fn)(lt_node *, lt_room, const uint8_t *, uint32_t, const char *, lt_blob *);
    HMODULE library = GetModuleHandleA("lantern.dll");
    union { FARPROC raw; create_fn call; } create = { .raw = GetProcAddress(library, "lt_channel_create_sealed") };
    union { FARPROC raw; reseal_fn call; } reseal = { .raw = GetProcAddress(library, "lt_channel_reseal") };
    union { FARPROC raw; add_fn call; } add = { .raw = GetProcAddress(library, "lt_blob_add_sealed_bytes") };
    lt_room room_a = 0, room_b = 0, next = 0;
    lt_blob local = 0, remote = 0;
    char ticket[4096], next_ticket[4096]; size_t length = 0, next_length = 0;
    uint8_t original[2210]; uint32_t original_length = 0;
    const char *saved = "sealed-saved.download", *active = "sealed-active.download";
    CHECK(create.call && reseal.call && add.call, "sealed transfer exports");
    CHECK(create.call(a, "private Windows channel", &room_a) == LT_OK &&
        wait_event(a, LT_EV_ROOM_JOINED, room_a, NULL), "sealed creator joined");
    CHECK(lt_room_history_enable(a, room_a, 1) == LT_OK &&
        lt_room_send_with_history(a, room_a, (const uint8_t *)"Windows sealed original", 23) == LT_OK &&
        wait_event(a, LT_EV_CHANNEL_RECORD, room_a, NULL), "sealed original history captured");
    original_length = last_record_length; memcpy(original, last_record, original_length);
    CHECK(original_length == 185 && !memcmp(original, "LTHREC01", 8) && !memcmp(original + 8, aid, 32), "original signed history shape");
    CHECK(lt_room_ticket(a, room_a, ticket, sizeof ticket, &length) == LT_OK &&
        length > 8 && !memcmp(ticket, "ltsealed", 8), "sealed ticket");
    CHECK(lt_room_join(b, LT_SCOPE_CUSTOM, "", ticket, &room_b) == LT_OK &&
        lt_room_history_enable(b, room_b, 1) == LT_OK, "sealed peer join queued using localhost hints");
    CHECK(wait_event(b, LT_EV_CHANNEL_HISTORY, room_b, NULL) && last_record_length == original_length &&
        !memcmp(last_record, original, original_length), "actual encrypted history snapshot preserves original signature");
    puts("PASS: Windows sealed peers exchange encrypted history with the original author signature");
    expected_offer_room = room_b;
    CHECK(add.call(a, room_a, bytes, 65537, "clear.png", &local) == LT_OK &&
        wait_event(a, LT_EV_BLOB_DONE, local, NULL), "encrypted Windows file import");
    CHECK(lt_blob_offer(a, room_a, local) == LT_OK &&
        wait_event(b, LT_EV_BLOB_OFFER, 0, &remote), "sealed Windows file offer");
    CHECK(GetFileAttributesA(saved) == INVALID_FILE_ATTRIBUTES &&
        lt_blob_fetch(b, remote, saved) == LT_E_STATE, "no auto-download or generic ciphertext export");
    CHECK(lt_blob_fetch_bounded(b, remote, saved) == LT_OK &&
        wait_event(b, LT_EV_BLOB_DONE, remote, NULL) && exact_file(saved, bytes, 65537), "actual Windows authenticated decrypted transfer");
    CHECK(lt_blob_fetch_bounded(b, remote, saved) == LT_OK && terminal(remote) == 2 &&
        exact_file(saved, bytes, 65537), "sealed overwrite refusal");
    CHECK(lt_blob_unoffer(a, room_a, local) == LT_OK &&
        wait_event(b, LT_EV_BLOB_WITHDRAWN, remote, NULL) &&
        lt_blob_fetch_bounded(b, remote, active) == LT_E_NOT_FOUND && exact_file(saved, bytes, 65537), "sealed withdrawal preserves saved file");
    CHECK(reseal.call(a, room_a, &next) == LT_OK &&
        wait_event(a, LT_EV_ROOM_JOINED, next, NULL) &&
        lt_room_ticket(a, next, next_ticket, sizeof next_ticket, &next_length) == LT_OK &&
        (length != next_length || memcmp(ticket, next_ticket, length)) &&
        lt_blob_offer(a, next, local) == LT_E_STATE, "new-key file reuse refused");
    puts("PASS: Windows sealed exact-byte decrypted download, overwrite refusal, withdrawal and new-key isolation");
    CHECK(add.call(a, room_a, bytes, (uint32_t)size, "clear.png", &local) == LT_OK &&
        wait_event(a, LT_EV_BLOB_DONE, local, NULL) && lt_blob_offer(a, room_a, local) == LT_OK &&
        wait_event(b, LT_EV_BLOB_OFFER, 0, &remote), "maximum encrypted file offer");
    CHECK(lt_blob_fetch_bounded(b, remote, active) == LT_OK, "queue maximum encrypted download");
    lt_status canceled = lt_blob_cancel_bounded(b, remote);
    CHECK(canceled == LT_OK || canceled == LT_E_NOT_FOUND, "encrypted cancellation request");
    int result = terminal(remote);
    CHECK(result == 1 ? exact_file(active, bytes, size) : result == 2 &&
        GetFileAttributesA(active) == INVALID_FILE_ATTRIBUTES, "encrypted cancellation intact or no partial");
    CHECK(lt_blob_unoffer(a, room_a, local) == LT_OK && wait_event(b, LT_EV_BLOB_WITHDRAWN, remote, NULL), "canceled encrypted offer withdrawal");
    puts("PASS: Windows sealed 16 MiB cancellation yields intact completion or no partial file");
cleanup:
    expected_offer_room = 0;
    if (room_b) lt_room_leave(b, room_b);
    if (room_a) lt_room_leave(a, room_a);
    if (next) lt_room_leave(a, next);
    SecureZeroMemory(ticket, sizeof ticket); SecureZeroMemory(next_ticket, sizeof next_ticket);
    DeleteFileA(saved); DeleteFileA(active);
    return failure == NULL;
}

int main(int argc, char **argv) {
    uint8_t *bytes = NULL;
    const char *saved = "friend-saved.download", *active = "friend-active.download";
    lt_config cfg = {0}; cfg.abi_version = LT_ABI_VERSION;
    cfg.app_id = "lantern-wine-file-transfer/1"; cfg.relay_mode = LT_RELAY_DISABLED; cfg.accept_inbound = 1;
    CHECK(lt_node_open(&cfg, &a) == LT_OK && lt_node_open(&cfg, &b) == LT_OK, "open private nodes");
    CHECK(lt_node_id(a, aid) == LT_OK && lt_node_id(b, bid) == LT_OK, "identities");
    char invite[LT_MAX_TEXT]; size_t length = 0; uint64_t handle = 0;
    /* Give asynchronous interface discovery a direct self-check before minting
     * the invite; never print the invite's bearer secret. */
    CHECK(lt_selftest(a, &handle) == LT_OK && wait_event(a, LT_EV_SELFTEST, handle, NULL), "first endpoint warmup");
    CHECK(lt_selftest(b, &handle) == LT_OK && wait_event(b, LT_EV_SELFTEST, handle, NULL), "second endpoint warmup");
    CHECK(local_hint("a", aid, aport) && local_hint("b", bid, bport), "write public localhost hints");
    CHECK(receive_hint(b, "a") && receive_hint(a, "b"), "apply explicit private localhost hints");
    CHECK(lt_node_ticket(a, invite, sizeof invite, &length) == LT_OK, "address ticket diagnostic");
    printf("Windows first endpoint ticket length: %zu (contents omitted)\n", length);
    CHECK(lt_invite_create(a, 600, invite, sizeof invite, &length) == LT_OK && lt_invite_accept(b, invite, &handle) == LT_OK, "private friendship");
    int added[2] = {0}; ULONGLONG deadline = GetTickCount64() + 30000; lt_event events[32];
    while ((!added[0] || !added[1]) && GetTickCount64() < deadline) {
        lt_node *nodes[] = {a, b};
        for (int n = 0; n < 2; n++) {
            int count = lt_poll(nodes[n], events, 32);
            CHECK(count >= 0, "poll friendship");
            for (int i = 0; i < count; i++) {
                if (events[i].kind == LT_EV_INVITE_FAILED || events[i].kind == LT_EV_ERROR) {
                    fprintf(stderr, "Windows friend event %u: %.*s\n", events[i].kind, (int)events[i].data_len, events[i].data);
                    if (events[i].kind == LT_EV_INVITE_FAILED) { failure = "invite failed"; goto cleanup; }
                }
                if (events[i].kind == LT_EV_FRIEND_ADDED && !memcmp(events[i].peer, n ? aid : bid, 32)) added[n] = 1;
            }
        }
        Sleep(10);
    }
    CHECK(added[0] && added[1], "friendship timeout");
    const size_t size = 16 * 1024 * 1024;
    bytes = malloc(size); CHECK(bytes != NULL, "allocate capped fixture");
    for (size_t i = 0; i < size; i++) bytes[i] = (uint8_t)(i * 31 + 7);
    lt_blob local = 0, remote = 0;
    CHECK(lt_blob_add_bytes(a, bytes, 65537, "clear.png", &local) == LT_OK && wait_event(a, LT_EV_BLOB_DONE, local, NULL), "hash fixture");
    CHECK(lt_friend_file_offer(a, bid, local) == LT_OK && wait_event(b, LT_EV_BLOB_OFFER, 0, &remote), "authenticated friend offer");
    CHECK(GetFileAttributesA(saved) == INVALID_FILE_ATTRIBUTES, "no automatic download");
    CHECK(lt_blob_fetch_bounded(b, remote, saved) == LT_OK && wait_event(b, LT_EV_BLOB_DONE, remote, NULL), "explicit bounded transfer");
    CHECK(exact_file(saved, bytes, 65537), "exact saved bytes");
    CHECK(lt_blob_fetch_bounded(b, remote, saved) == LT_OK && terminal(remote) == 2 && exact_file(saved, bytes, 65537), "overwrite refusal preserves saved bytes");
    CHECK(lt_friend_file_withdraw(a, bid, local) == LT_OK && wait_event(b, LT_EV_BLOB_WITHDRAWN, remote, NULL), "scoped withdrawal");
    CHECK(lt_blob_fetch_bounded(b, remote, active) == LT_E_NOT_FOUND && exact_file(saved, bytes, 65537), "withdrawal invalidates handle and preserves saved file");
    puts("PASS: Windows private friend offer, explicit exact-byte download, overwrite refusal and withdrawal");
    if (argc > 1 && strcmp(argv[1], "1") == 0 && !sealed_transfer(bytes, size)) goto cleanup;
    CHECK(lt_blob_add_bytes(a, bytes, (uint32_t)size, "clear.png", &local) == LT_OK && wait_event(a, LT_EV_BLOB_DONE, local, NULL), "hash large fixture");
    CHECK(lt_friend_file_offer(a, bid, local) == LT_OK && wait_event(b, LT_EV_BLOB_OFFER, 0, &remote), "large friend offer");
    CHECK(lt_blob_fetch_bounded(b, remote, active) == LT_OK, "queue active transfer");
    lt_status canceled = lt_blob_cancel_bounded(b, remote);
    CHECK(canceled == LT_OK || canceled == LT_E_NOT_FOUND, "cancel request");
    int result = terminal(remote); CHECK(result != 0, "cancel terminal");
    CHECK(result == 1 ? exact_file(active, bytes, size) : GetFileAttributesA(active) == INVALID_FILE_ATTRIBUTES, "cancel completion or partial cleanup");
    CHECK(DeleteFileA(active) || GetFileAttributesA(active) == INVALID_FILE_ATTRIBUTES, "clear owned fixture");
    CHECK(lt_blob_fetch_bounded(b, remote, active) == LT_OK && lt_friend_remove(b, aid) == LT_OK, "unfriend during transfer");
    result = terminal(remote); CHECK(result != 0, "unfriend terminal");
    CHECK(result == 1 ? exact_file(active, bytes, size) : GetFileAttributesA(active) == INVALID_FILE_ATTRIBUTES, "unfriend completion or partial cleanup");
    CHECK(lt_blob_fetch_bounded(b, remote, "stale.download") == LT_E_NOT_FOUND, "unfriend invalidates handle");
    puts("PASS: Windows 16 MiB queued cancellation/unfriend yields intact completion or no partial file");
cleanup:
    if (b) lt_node_close(b);
    if (a) lt_node_close(a);
    free(bytes); DeleteFileA(saved); DeleteFileA(active);
    if (failure) { fprintf(stderr, "FAIL: %s\n", failure); return 1; }
    return 0;
}
