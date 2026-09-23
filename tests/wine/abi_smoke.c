/* abi_smoke.c: load linkpearl.dll the way a plugin would and prove, in this
 * process, that the endpoint binds and is reachable.
 *
 *   x86_64-w64-mingw32-gcc -O2 -o abi_smoke.exe abi_smoke.c -I include -L <dir> -llinkpearl
 *   wine abi_smoke.exe [off|default]      (default: n0 relays)
 *
 * Prints the build, the node id, a friend invite and the selftest report.
 * Exit 0 when the direct path works (and, with relays on, the relay path).
 * Uses a throwaway in-memory identity: nothing is written. */
#include <stdio.h>
#include <string.h>
#include <windows.h>
#include "linkpearl.h"

int main(int argc, char **argv) {
    int relays_off = argc > 1 && strcmp(argv[1], "off") == 0;
    printf("abi %u, %s\n", lp_abi_version(), lp_build_info());
    if (lp_abi_version() != LP_ABI_VERSION) { printf("ABI mismatch\n"); return 2; }

    lp_config cfg = {0};
    cfg.abi_version = LP_ABI_VERSION;
    cfg.app_id = "linkpearl-wine-smoke/1";
    cfg.relay_mode = relays_off ? LP_RELAY_DISABLED : LP_RELAY_DEFAULT;
    cfg.accept_inbound = 1;
    lp_node *node = NULL;
    lp_status st = lp_node_open(&cfg, &node);
    if (st != LP_OK) { printf("lp_node_open: %s\n", lp_status_text(st)); return 3; }

    char text[LP_MAX_TEXT];
    size_t n = 0;
    lp_node_id_text(node, text, sizeof text, &n);
    printf("node id: %s\n", text);
    st = lp_invite_create(node, 600, text, sizeof text, &n);
    printf("invite: %s (%zu chars, %s)\n", st == LP_OK ? "ok" : "failed", n, lp_status_text(st));

    uint64_t handle = 0;
    st = lp_selftest(node, &handle);
    if (st != LP_OK) { printf("lp_selftest: %s\n", lp_status_text(st)); return 4; }

    lp_event ev[32];
    int rc = 5;
    for (int tick = 0; tick < 600; tick++) {   /* 60 s */
        int got = lp_poll(node, ev, 32);
        for (int i = 0; i < got; i++) {
            if (ev[i].kind == LP_EV_SELFTEST && ev[i].handle == handle) {
                printf("selftest: %.*s\n", (int)ev[i].data_len, (const char *)ev[i].data);
                const char *r = (const char *)ev[i].data;
                int direct = strstr(r, "\"direct\":{\"ok\":true") != NULL;
                int relay = strstr(r, "\"relay\":{\"ok\":true") != NULL;
                rc = direct && (relays_off || relay) ? 0 : 1;
                goto done;
            }
            if (ev[i].kind == LP_EV_ERROR || ev[i].kind == LP_EV_LOG)
                printf("event %u: %.*s\n", ev[i].kind, (int)ev[i].data_len, (const char *)ev[i].data);
        }
        Sleep(100);
    }
    printf("selftest timed out\n");
done:
    lp_node_close(node);
    printf("closed, exit %d\n", rc);
    return rc;
}
