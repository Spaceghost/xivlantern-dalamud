/* Actual pre-tag and tagged libraries, loaded independently. Ephemeral
 * in-memory identities, relays disabled, no game or human recipients. */
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include "lantern.h"

#define FUNCTIONS(X) \
    X(lt_abi_version) X(lt_node_open) X(lt_node_close) X(lt_node_id) \
    X(lt_poll) X(lt_profile_set_name) X(lt_presence_status_set) \
    X(lt_invite_create) X(lt_invite_accept) X(lt_friend_send) X(lt_friend_list) \
    X(lt_room_join) X(lt_room_ticket) X(lt_room_send) \
    X(lt_blob_add_bytes) X(lt_blob_offer)

typedef struct {
#define FIELD(name) __typeof__(&name) name;
    FUNCTIONS(FIELD)
#undef FIELD
    __typeof__(&lt_activity_tags_set) tags;
    __typeof__(&lt_room_send_with_history) history_send;
    __typeof__(&lt_channel_record_events_enable) metadata;
    __typeof__(&lt_blob_cancel_bounded) bounded_cancel;
    __typeof__(&lt_friend_file_offer) friend_file_offer;
    lt_node *node;
    uint8_t id[32];
} api;

static void require(int condition, const char *message) {
    if (!condition) { fprintf(stderr, "%s\n", message); exit(1); }
}

static api load(const char *path, const char *name) {
    api a = {0};
    void *lib = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    require(lib != NULL, "Native library did not load");
#define RESOLVE(symbol) a.symbol = (__typeof__(a.symbol))dlsym(lib, #symbol); require(a.symbol != NULL, "Missing " #symbol);
    FUNCTIONS(RESOLVE)
#undef RESOLVE
    a.tags = (__typeof__(a.tags))dlsym(lib, "lt_activity_tags_set");
    a.history_send = (__typeof__(a.history_send))dlsym(lib, "lt_room_send_with_history");
    a.metadata = (__typeof__(a.metadata))dlsym(lib, "lt_channel_record_events_enable");
    a.bounded_cancel = (__typeof__(a.bounded_cancel))dlsym(lib, "lt_blob_cancel_bounded");
    a.friend_file_offer = (__typeof__(a.friend_file_offer))dlsym(lib, "lt_friend_file_offer");
    require(a.lt_abi_version() == LT_ABI_VERSION, "ABI mismatch");
    lt_config config = {0};
    config.abi_version = LT_ABI_VERSION;
    config.app_id = "lantern-mixed-version-test/1";
    config.relay_mode = LT_RELAY_DISABLED;
    config.accept_inbound = 1;
    require(a.lt_node_open(&config, &a.node) == LT_OK, "Node open failed");
    require(a.lt_node_id(a.node, a.id) == LT_OK, "Node identity failed");
    require(a.lt_profile_set_name(a.node, name) == LT_OK, "Name failed");
    // Keep each library loaded until process exit: no unloading Rust worker
    // code while asynchronous endpoint shutdown may still be completing.
    return a;
}

static double seconds(void) {
    struct timespec t;
    require(clock_gettime(CLOCK_MONOTONIC, &t) == 0, "Clock failed");
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int text_is(const lt_event *event, const char *text) {
    size_t length = strlen(text);
    return event->data_len == length && event->data && memcmp(event->data, text, length) == 0;
}

static int from(const lt_event *event, const api *peer) {
    return memcmp(event->peer, peer->id, sizeof peer->id) == 0;
}

static int poll(api *a, lt_event *events) {
    int count = a->lt_poll(a->node, events, 32);
    require(count >= 0 && count <= 32, "Poll failed");
    return count;
}

/* A real archived legacy library must receive only the original text,
 * including repeated text signed with distinct record identities. */
static void history_compatibility(api *old, api *current) {
    if (!current->history_send) return; /* Existing activity-only fixture. */
    require(!old->history_send && current->metadata, "Expected legacy/history pair");
    require(current->metadata(current->node, 1) == LT_OK, "Metadata opt-in failed");
    lt_room old_room = 0, new_room = 0;
    require(old->lt_room_join(old->node, LT_SCOPE_CUSTOM, "mixed-history", NULL, &old_room) == LT_OK, "Legacy room failed");
    double deadline = seconds() + 20;
    lt_event events[32];
    int joined = 0;
    while (!joined && seconds() < deadline) {
        int n = poll(old, events);
        for (int i = 0; i < n; i++)
            if (events[i].kind == LT_EV_ROOM_JOINED && events[i].handle == old_room) joined = 1;
        usleep(10000);
    }
    require(joined, "Legacy room never joined");
    char ticket[LT_MAX_TEXT]; size_t needed = 0;
    require(old->lt_room_ticket(old->node, old_room, ticket, sizeof ticket, &needed) == LT_OK, "Room ticket failed");
    require(current->lt_room_join(current->node, LT_SCOPE_CUSTOM, "", ticket, &new_room) == LT_OK, "History room failed");
    int connected[2] = {0}; deadline = seconds() + 20;
    while ((!connected[0] || !connected[1]) && seconds() < deadline) {
        int n = poll(old, events);
        for (int i = 0; i < n; i++)
            if (events[i].kind == LT_EV_PEER_JOINED && events[i].handle == old_room && from(&events[i], current)) connected[0] = 1;
        n = poll(current, events);
        for (int i = 0; i < n; i++)
            if (events[i].kind == LT_EV_PEER_JOINED && events[i].handle == new_room && from(&events[i], old)) connected[1] = 1;
        usleep(10000);
    }
    require(connected[0] && connected[1], "Mixed rooms did not connect");
    const char *text = "identical signed channel text";
    uint8_t previous_signature[64] = {0};
    for (int repeat = 0; repeat < 2; repeat++) {
        require(current->history_send(current->node, new_room, (const uint8_t *)text, strlen(text)) == LT_OK, "Signed room send failed");
        int delivered = 0, own = 0; deadline = seconds() + 20;
        while ((!delivered || !own) && seconds() < deadline) {
            int n = poll(old, events);
            for (int i = 0; i < n; i++)
                if (events[i].kind == LT_EV_MESSAGE && events[i].handle == old_room && from(&events[i], current)) {
                    require(text_is(&events[i], text), "Legacy saw record framing instead of clean text");
                    require(!delivered, "Duplicate legacy text"); delivered = 1;
                }
            n = poll(current, events);
            for (int i = 0; i < n; i++)
                if (events[i].kind == LT_EV_CHANNEL_RECORD && events[i].handle == new_room && from(&events[i], current)) {
                    require(events[i].data_len == 162 + strlen(text), "Invalid original record length");
                    require(memcmp(events[i].data, "LTHREC01", 8) == 0, "Invalid record header");
                    const uint8_t *sig = events[i].data + events[i].data_len - 64;
                    if (repeat) require(memcmp(previous_signature, sig, 64) != 0, "Repeated text lost distinct signatures");
                    memcpy(previous_signature, sig, 64); own = 1;
                }
            usleep(10000);
        }
        require(delivered && own, "Signed text did not cross actual legacy/history libraries");
    }
    text = "ordinary legacy room text";
    require(old->lt_room_send(old->node, old_room, (const uint8_t *)text, strlen(text)) == LT_OK, "Legacy room send failed");
    int received = 0; deadline = seconds() + 20;
    while (!received && seconds() < deadline) {
        int n = poll(current, events);
        for (int i = 0; i < n; i++)
            if (events[i].kind == LT_EV_MESSAGE && events[i].handle == new_room && from(&events[i], old)) {
                require(text_is(&events[i], text), "Legacy plain text changed"); received = 1;
            }
        poll(old, events); usleep(10000);
    }
    require(received, "History library did not receive legacy channel text");
    if (current->bounded_cancel) {
        lt_blob blob = 0;
        const uint8_t payload[] = {1, 2, 3};
        require(current->lt_blob_add_bytes(current->node, payload, sizeof payload, "mixed-file.png", &blob) == LT_OK, "File add failed");
        int hashed = 0; deadline = seconds() + 20;
        while (!hashed && seconds() < deadline) {
            int n = poll(current, events);
            for (int i = 0; i < n; i++)
                if (events[i].kind == LT_EV_BLOB_DONE && events[i].handle == blob) hashed = 1;
            poll(old, events); usleep(10000);
        }
        require(hashed, "File hashing never completed");
        for (int repeat = 0; repeat < 2; repeat++) {
            require(current->lt_blob_offer(current->node, new_room, blob) == LT_OK, "File offer failed");
            int offered = 0; deadline = seconds() + 20;
            while (!offered && seconds() < deadline) {
                int n = poll(old, events);
                for (int i = 0; i < n; i++)
                    if (events[i].kind == LT_EV_BLOB_OFFER && from(&events[i], current)) offered = 1;
                poll(current, events); usleep(10000);
            }
            require(offered, "Repeated file offer did not reach actual legacy library");
        }
        puts("PASS: repeated signed file offers reach actual legacy library");
    }
    puts("PASS: signed repeated channel text reaches actual legacy library cleanly; legacy channel text reaches history library");
}

int main(int argc, char **argv) {
    require(argc == 3, "Expected pre-tag and tagged library paths");
    api old = load(argv[1], "Legacy");
    api tagged = load(argv[2], "Tagged");
    require(old.tags == NULL && tagged.tags != NULL, "Test requires distinct pre-tag and tagged versions");
    char invite[LT_MAX_TEXT];
    size_t length = 0;
    uint64_t handle = 0;
    require(old.lt_invite_create(old.node, 600, invite, sizeof invite, &length) == LT_OK, "Invite failed");
    require(tagged.lt_invite_accept(tagged.node, invite, &handle) == LT_OK, "Accept failed");
    int added[2] = {0};
    double deadline = seconds() + 30;
    lt_event events[32];
    while ((!added[0] || !added[1]) && seconds() < deadline) {
        int n = poll(&old, events);
        for (int i = 0; i < n; i++)
            if (events[i].kind == LT_EV_FRIEND_ADDED && from(&events[i], &tagged)) added[0] = 1;
        n = poll(&tagged, events);
        for (int i = 0; i < n; i++)
            if (events[i].kind == LT_EV_FRIEND_ADDED && from(&events[i], &old)) added[1] = 1;
        usleep(10000);
    }
    require(added[0] && added[1], "Mixed-version friendship did not complete");
    if (tagged.friend_file_offer) {
        require(old.friend_file_offer == NULL, "Expected legacy peer without friend files");
        const uint8_t payload[] = {4, 5, 6};
        lt_blob blob = 0;
        require(tagged.lt_blob_add_bytes(tagged.node, payload, sizeof payload, "friend-compat.png", &blob) == LT_OK, "Friend file add failed");
        int hashed = 0; deadline = seconds() + 20;
        while (!hashed && seconds() < deadline) {
            int n = poll(&tagged, events);
            for (int i = 0; i < n; i++)
                if (events[i].kind == LT_EV_BLOB_DONE && events[i].handle == blob) hashed = 1;
            poll(&old, events); usleep(10000);
        }
        require(hashed, "Friend file hashing never completed");
        require(tagged.friend_file_offer(tagged.node, old.id, blob) == LT_OK, "Legacy-target friend offer failed");
        /* Allow the independent stream to run before exercising chat. This
         * observes compatibility, not receipt of the unknown legacy frame. */
        deadline = seconds() + 1;
        while (seconds() < deadline) {
            int n = poll(&old, events);
            for (int i = 0; i < n; i++)
                require(events[i].kind != LT_EV_BLOB_OFFER, "Legacy peer interpreted unsupported friend file");
            poll(&tagged, events); usleep(10000);
        }
    }
    require(tagged.tags(tagged.node, "[\"maps\",\"custom activity\"]") == LT_OK, "Tags failed");
    require(tagged.lt_presence_status_set(tagged.node, LT_PRESENCE_AWAY, "brb") == LT_OK, "Presence failed");
    const char *to_old = "new after tags", *to_new = "legacy after tags";
    require(tagged.lt_friend_send(tagged.node, old.id, (const uint8_t *)to_old, strlen(to_old), &handle) == LT_OK, "Tagged send failed");
    require(old.lt_friend_send(old.node, tagged.id, (const uint8_t *)to_new, strlen(to_new), &handle) == LT_OK, "Legacy send failed");
    int presence = 0, received[2] = {0};
    deadline = seconds() + 30;
    while ((!presence || !received[0] || !received[1]) && seconds() < deadline) {
        int n = poll(&old, events);
        for (int i = 0; i < n; i++) {
            if (!from(&events[i], &tagged)) continue;
            if (events[i].kind == LT_EV_FRIEND_ONLINE && events[i].handle == LT_PRESENCE_AWAY && text_is(&events[i], "brb")) presence = 1;
            if (events[i].kind == LT_EV_FRIEND_TEXT && text_is(&events[i], to_old)) received[0] = 1;
        }
        n = poll(&tagged, events);
        for (int i = 0; i < n; i++)
            if (events[i].kind == LT_EV_FRIEND_TEXT && from(&events[i], &old) && text_is(&events[i], to_new)) received[1] = 1;
        usleep(10000);
    }
    require(presence && received[0] && received[1], "Tag extension disrupted legacy presence or bidirectional chat");
    if (tagged.friend_file_offer) puts("PASS: chat and presence continue after friend-file offer queued to actual legacy peer (no receipt acknowledgment)");
    char friends[LT_MAX_TEXT];
    require(tagged.lt_friend_list(tagged.node, friends, sizeof friends, &length) == LT_OK, "Roster failed");
    require(strstr(friends, "\"tags\":[]") != NULL, "Legacy peer should have no activities");
    history_compatibility(&old, &tagged);
    old.lt_node_close(old.node);
    tagged.lt_node_close(tagged.node);
    puts("PASS: distinct legacy/tagged libraries befriend, preserve presence and chat both ways after activity frames");
    return 0;
}
