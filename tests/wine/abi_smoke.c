/* abi_smoke.c: load lantern.dll the way a plugin would and prove, in this
 * process, that the endpoint binds and is reachable.
 *
 *   x86_64-w64-mingw32-gcc -O2 -o abi_smoke.exe abi_smoke.c -I include -L <dir> -llantern
 *   wine abi_smoke.exe [off|default]      (default: n0 relays)
 *
 * Prints the build, the node id, a friend invite and the selftest report.
 * Exit 0 when the direct path works (and, with relays on, the relay path).
 * Uses a throwaway in-memory identity: nothing is written. */
#include <stdio.h>
#include <string.h>
#include <windows.h>
#include "lantern.h"

/* Event data is length-delimited, not a NUL-terminated C string. */
static int contains(const uint8_t *data, size_t length, const char *needle) {
    size_t n = strlen(needle);
    if (!data || n > length) return 0;
    for (size_t i = 0; i <= length - n; i++)
        if (memcmp(data + i, needle, n) == 0) return 1;
    return 0;
}

/* Copy event bytes immediately: the native poll buffer is reused. */
static int wait_room_event(lt_node *node, lt_room room, uint32_t kind,
                           uint8_t *bytes, uint32_t capacity, uint32_t *length) {
    lt_event events[32];
    for (int tick = 0; tick < 2000; tick++) {
        int got = lt_poll(node, events, 32);
        if (got < 0) return 0;
        for (int i = 0; i < got; i++) {
            if (events[i].kind != kind || events[i].handle != room) continue;
            if (events[i].data_len > capacity) return 0;
            *length = events[i].data_len;
            if (*length) {
                if (!events[i].data || !bytes) return 0;
                memcpy(bytes, events[i].data, *length);
            }
            return 1;
        }
        Sleep(10);
    }
    return 0;
}

static int history_smoke(lt_node *node) {
    lt_room room = 0;
    uint32_t length = 0, saved_length = 0;
    uint8_t record[2210], saved[2210], author[32];
    const char *text = "Windows signed channel fixture";
    if (lt_channel_record_events_enable(node, 2) != LT_E_ARG ||
        lt_channel_record_events_enable(node, 1) != LT_OK ||
        lt_node_id(node, author) != LT_OK ||
        lt_room_join(node, LT_SCOPE_CUSTOM, "windows-history-fixture", NULL, &room) != LT_OK ||
        !wait_room_event(node, room, LT_EV_ROOM_JOINED, NULL, 0, &length) ||
        lt_room_history_enable(node, room, 1) != LT_OK ||
        lt_room_send_with_history(node, room, (const uint8_t *)text, (uint32_t)strlen(text)) != LT_OK ||
        !wait_room_event(node, room, LT_EV_CHANNEL_RECORD, record, sizeof record, &length)) return 6;
    if (length != 162 + strlen(text) || memcmp(record, "LTHREC01", 8) ||
        memcmp(record + 8, author, 32) || memcmp(record + 98, text, strlen(text))) return 7;
    if (lt_room_history_enable(node, room, 1) != LT_OK ||
        !wait_room_event(node, room, LT_EV_CHANNEL_HISTORY, saved, sizeof saved, &saved_length) ||
        saved_length != length || memcmp(record, saved, length)) return 8;
    if (lt_room_history_clear(node, room) != LT_OK ||
        !wait_room_event(node, room, LT_EV_CHANNEL_HISTORY_CLEARED, NULL, 0, &saved_length) ||
        saved_length != 0) return 9;
    text = "Windows line after local clear";
    if (lt_room_send_with_history(node, room, (const uint8_t *)text, (uint32_t)strlen(text)) != LT_OK ||
        !wait_room_event(node, room, LT_EV_CHANNEL_RECORD, record, sizeof record, &length) ||
        lt_room_history_enable(node, room, 1) != LT_OK ||
        !wait_room_event(node, room, LT_EV_CHANNEL_HISTORY, saved, sizeof saved, &saved_length) ||
        length != 162 + strlen(text) || saved_length != length ||
        memcmp(record + 98, text, strlen(text)) || memcmp(record, saved, length)) return 10;
    puts("PASS: Windows signed metadata, exact saved history and acknowledged local clear");
    return 0;
}

static int name_card_smoke(lt_node *node) {
    typedef lt_status (*identity_fn)(lt_node *, char *, size_t, size_t *);
    typedef lt_status (*send_fn)(lt_node *, const uint8_t *, uint32_t, const char *, uint32_t, uint64_t *);
    typedef lt_status (*inbox_fn)(lt_node *, const uint8_t *, uint32_t, uint64_t *);
    HMODULE library = GetModuleHandleA("lantern.dll");
    if (!library) return 10;
    union { FARPROC raw; identity_fn call; } identity = { .raw = GetProcAddress(library, "lt_nostr_identity") };
    union { FARPROC raw; send_fn call; } send = { .raw = GetProcAddress(library, "lt_nostr_send_invite") };
    union { FARPROC raw; inbox_fn call; } inbox = { .raw = GetProcAddress(library, "lt_nostr_check_inbox") };
    if (!identity.raw || !send.raw || !inbox.raw) return 11;
    char first[128] = {0}, second[128] = {0};
    size_t needed = 0;
    if (identity.call(node, first, sizeof(first), &needed) != LT_OK || needed >= sizeof(first)
        || strncmp(first, "npub1", 5) != 0) return 12;
    if (identity.call(node, second, sizeof(second), &needed) != LT_OK || strcmp(first, second)) return 13;
    uint64_t handle = 999;
    const char *relay = "ws://127.0.0.1:1";
    /* Invalid arguments are refused before any job or connection is created. */
    if (send.call(node, (const uint8_t *)relay, (uint32_t)strlen(relay), first, 0, &handle) != LT_E_ARG
        || handle != 0) return 14;
    handle = 999;
    if (inbox.call(node, NULL, 0, &handle) != LT_E_ARG || handle != 0) return 15;
    puts("PASS: Windows name-card exports, stable local public identity and invalid-job rejection; no relay contact");
    return 0;
}

static int bounded_file_smoke(lt_node *node) {
    typedef lt_status (*fetch_fn)(lt_node *, lt_blob, const char *);
    typedef lt_status (*cancel_fn)(lt_node *, lt_blob);
    HMODULE library = GetModuleHandleA("lantern.dll");
    if (!library) return 16;
    union { FARPROC raw; fetch_fn call; } fetch = { .raw = GetProcAddress(library, "lt_blob_fetch_bounded") };
    union { FARPROC raw; cancel_fn call; } cancel = { .raw = GetProcAddress(library, "lt_blob_cancel_bounded") };
    if (!fetch.raw || !cancel.raw) return 17;
    if (fetch.call(NULL, 0, "unused.download") != LT_E_ARG ||
        fetch.call(node, 0, NULL) != LT_E_ARG ||
        cancel.call(NULL, 0) != LT_E_ARG) return 18;
    /* Unknown handles cannot create files, connections or cancellation jobs. */
    if (fetch.call(node, UINT64_MAX, "unused.download") != LT_E_NOT_FOUND ||
        cancel.call(node, UINT64_MAX) != LT_E_NOT_FOUND ||
        GetFileAttributesA("unused.download") != INVALID_FILE_ATTRIBUTES) return 19;
    puts("PASS: Windows bounded file exports and invalid/unknown job rejection; no file or relay contact");
    return 0;
}

static int friend_file_smoke(lt_node *node) {
    typedef lt_status (*offer_fn)(lt_node *, const uint8_t *, lt_blob);
    HMODULE library = GetModuleHandleA("lantern.dll");
    if (!library) return 20;
    union { FARPROC raw; offer_fn call; } offer = { .raw = GetProcAddress(library, "lt_friend_file_offer") };
    union { FARPROC raw; offer_fn call; } withdraw = { .raw = GetProcAddress(library, "lt_friend_file_withdraw") };
    if (!offer.raw || !withdraw.raw) return 21;
    uint8_t peer[LT_NODE_ID_LEN];
    if (lt_node_id(node, peer) != LT_OK) return 22;
    if (offer.call(NULL, peer, 0) != LT_E_ARG || offer.call(node, NULL, 0) != LT_E_ARG
        || withdraw.call(NULL, peer, 0) != LT_E_ARG || withdraw.call(node, NULL, 0) != LT_E_ARG) return 23;
    /* Our own valid identity is not a friend. No frame is queued. */
    if (offer.call(node, peer, UINT64_MAX) != LT_E_NOT_FOUND
        || withdraw.call(node, peer, UINT64_MAX) != LT_E_NOT_FOUND) return 24;
    puts("PASS: Windows friend-file exports, null input and unknown friend rejection; no friend contact");
    return 0;
}

static int sealed_smoke(lt_node *node) {
    typedef lt_status (*create_fn)(lt_node *, const char *, lt_room *);
    typedef lt_status (*reseal_fn)(lt_node *, lt_room, lt_room *);
    typedef lt_status (*mode_fn)(lt_node *, lt_room, uint32_t *);
    typedef lt_status (*bytes_fn)(lt_node *, lt_room, const uint8_t *, uint32_t, const char *, lt_blob *);
    HMODULE library = GetModuleHandleA("lantern.dll");
    union { FARPROC raw; create_fn call; } create = { .raw = GetProcAddress(library, "lt_channel_create_sealed") };
    union { FARPROC raw; reseal_fn call; } reseal = { .raw = GetProcAddress(library, "lt_channel_reseal") };
    union { FARPROC raw; mode_fn call; } mode = { .raw = GetProcAddress(library, "lt_room_is_sealed") };
    union { FARPROC raw; bytes_fn call; } add = { .raw = GetProcAddress(library, "lt_blob_add_sealed_bytes") };
    if (!create.call || !reseal.call || !mode.call || !add.call) return 40;
    lt_room room = 0, next = 0;
    lt_blob blob = 0;
    uint32_t sealed = 99, length = 0;
    size_t ticket_length = 0, next_length = 0;
    char ticket[4096], next_ticket[4096];
    const uint8_t bytes[] = { 7, 8, 9 };
    if (create.call(NULL, "private", &room) != LT_E_ARG || room != 0 ||
        mode.call(node, 0, &sealed) != LT_E_NOT_FOUND || sealed != 0 ||
        create.call(node, "private", &room) != LT_OK ||
        !wait_room_event(node, room, LT_EV_ROOM_JOINED, NULL, 0, &length) ||
        mode.call(node, room, &sealed) != LT_OK || sealed != 1 ||
        lt_room_ticket(node, room, ticket, sizeof ticket, &ticket_length) != LT_OK ||
        ticket_length < 8 || memcmp(ticket, "ltsealed", 8) ||
        add.call(node, room, NULL, 16u * 1024u * 1024u + 1u, "file", &blob) != LT_E_TOO_LARGE || blob != 0 ||
        add.call(node, room, bytes, sizeof bytes, "private.bin", &blob) != LT_OK) return 41;
    uint8_t hash[128];
    if (!wait_room_event(node, blob, LT_EV_BLOB_DONE, hash, sizeof hash, &length) || length != 64 ||
        lt_blob_offer(node, room, blob) != LT_OK ||
        lt_blob_unoffer(node, room, blob) != LT_OK ||
        reseal.call(node, room, &next) != LT_OK || next == room ||
        !wait_room_event(node, next, LT_EV_ROOM_JOINED, NULL, 0, &length) ||
        lt_room_ticket(node, next, next_ticket, sizeof next_ticket, &next_length) != LT_OK ||
        (ticket_length == next_length && !memcmp(ticket, next_ticket, ticket_length)) ||
        lt_blob_offer(node, next, blob) != LT_E_STATE ||
        lt_room_leave(node, room) != LT_OK || lt_room_leave(node, next) != LT_OK) return 42;
    SecureZeroMemory(ticket, sizeof ticket);
    SecureZeroMemory(next_ticket, sizeof next_ticket);
    puts("PASS: Windows sealed exports, private ticket, encrypted import and fresh-key reseal; no remote channel contact");
    return 0;
}

int main(int argc, char **argv) {
    int relays_off = argc > 1 && strcmp(argv[1], "off") == 0;
    printf("abi %u, %s\n", lt_abi_version(), lt_build_info());
    if (lt_abi_version() != LT_ABI_VERSION) { printf("ABI mismatch\n"); return 2; }

    lt_config cfg = {0};
    cfg.abi_version = LT_ABI_VERSION;
    cfg.app_id = "lantern-wine-smoke/1";
    cfg.relay_mode = relays_off ? LT_RELAY_DISABLED : LT_RELAY_DEFAULT;
    cfg.accept_inbound = 1;
    lt_node *node = NULL;
    lt_status st = lt_node_open(&cfg, &node);
    if (st != LT_OK) { printf("lt_node_open: %s\n", lt_status_text(st)); return 3; }

    char text[LT_MAX_TEXT];
    size_t n = 0;
    lt_node_id_text(node, text, sizeof text, &n);
    printf("node id: %s\n", text);
    st = lt_invite_create(node, 600, text, sizeof text, &n);
    printf("invite: %s (%zu chars, %s)\n", st == LT_OK ? "ok" : "failed", n, lt_status_text(st));

    uint64_t handle = 0;
    st = lt_selftest(node, &handle);
    if (st != LT_OK) { printf("lt_selftest: %s\n", lt_status_text(st)); return 4; }

    lt_event ev[32];
    int rc = 5;
    for (int tick = 0; tick < 600; tick++) {   /* 60 s */
        int got = lt_poll(node, ev, 32);
        for (int i = 0; i < got; i++) {
            if (ev[i].kind == LT_EV_SELFTEST && ev[i].handle == handle) {
                printf("selftest: %.*s\n", (int)ev[i].data_len, (const char *)ev[i].data);
                int direct = contains(ev[i].data, ev[i].data_len, "\"direct\":{\"ok\":true");
                int relay = contains(ev[i].data, ev[i].data_len, "\"relay\":{\"ok\":true");
                rc = direct && (relays_off || relay) ? 0 : 1;
                goto done;
            }
            if (ev[i].kind == LT_EV_ERROR || ev[i].kind == LT_EV_LOG)
                printf("event %u: %.*s\n", ev[i].kind, (int)ev[i].data_len, (const char *)ev[i].data);
        }
        Sleep(100);
    }
    printf("selftest timed out\n");
done:
    if (rc == 0) rc = history_smoke(node);
    if (rc == 0 && argc > 2 && strcmp(argv[2], "1") == 0) rc = name_card_smoke(node);
    if (rc == 0 && argc > 3 && strcmp(argv[3], "1") == 0) rc = bounded_file_smoke(node);
    if (rc == 0 && argc > 4 && strcmp(argv[4], "1") == 0) rc = friend_file_smoke(node);
    if (rc == 0 && argc > 5 && strcmp(argv[5], "1") == 0) rc = sealed_smoke(node);
    lt_node_close(node);
    printf("closed, exit %d\n", rc);
    return rc;
}
