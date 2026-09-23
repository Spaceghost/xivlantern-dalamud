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
                const char *r = (const char *)ev[i].data;
                int direct = strstr(r, "\"direct\":{\"ok\":true") != NULL;
                int relay = strstr(r, "\"relay\":{\"ok\":true") != NULL;
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
    lt_node_close(node);
    printf("closed, exit %d\n", rc);
    return rc;
}
