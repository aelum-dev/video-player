/* Synthesize a real drag-and-drop gesture via uinput (companion of uiclick.c):
 * walk to (X1,Y1), press BTN_LEFT, walk to (X2,Y2) in small steps (this is
 * what starts a Wayland DnD), hold at the target so a screen capture can
 * catch the drop feedback, then release.
 * usage: udrag X1 Y1 X2 Y2 [HOLD_MS] — logical coordinates. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <linux/uinput.h>
#include <sys/ioctl.h>

static void ev(int fd, int type, int code, int val) {
    struct input_event e;
    memset(&e, 0, sizeof(e));
    e.type = (short)type; e.code = (short)code; e.value = val;
    if (write(fd, &e, sizeof(e)) < 0) perror("write");
}

static int cursor_pos(int *x, int *y) {
    FILE *p = popen("hyprctl cursorpos 2>/dev/null", "r");
    if (!p) return -1;
    int got = fscanf(p, "%d, %d", x, y);
    pclose(p);
    return got == 2 ? 0 : -1;
}

/* walk toward (tx,ty) reading the real cursor position between steps;
   step caps the motion per event so the drag gesture is gradual. */
static void walk(int fd, int tx, int ty, int step) {
    for (int i = 0; i < 300; i++) {
        int cx, cy;
        if (cursor_pos(&cx, &cy) < 0) break;
        int dx = tx - cx, dy = ty - cy;
        if (dx > -3 && dx < 3 && dy > -3 && dy < 3) break;
        if (dx > step) dx = step; else if (dx < -step) dx = -step;
        if (dy > step) dy = step; else if (dy < -step) dy = -step;
        ev(fd, EV_REL, REL_X, dx); ev(fd, EV_REL, REL_Y, dy);
        ev(fd, EV_SYN, SYN_REPORT, 0);
        usleep(25000);
    }
}

int main(int argc, char **argv) {
    if (argc < 5) { fprintf(stderr, "usage: udrag X1 Y1 X2 Y2 [HOLD_MS]\n"); return 1; }
    int x1 = atoi(argv[1]), y1 = atoi(argv[2]);
    int x2 = atoi(argv[3]), y2 = atoi(argv[4]);
    int hold = argc > 5 ? atoi(argv[5]) : 1500;
    int fd = open("/dev/uinput", O_WRONLY | O_NONBLOCK);
    if (fd < 0) { perror("open uinput"); return 1; }
    ioctl(fd, UI_SET_EVBIT, EV_KEY);
    ioctl(fd, UI_SET_KEYBIT, BTN_LEFT);
    ioctl(fd, UI_SET_EVBIT, EV_REL);
    ioctl(fd, UI_SET_RELBIT, REL_X);
    ioctl(fd, UI_SET_RELBIT, REL_Y);
    struct uinput_user_dev u;
    memset(&u, 0, sizeof(u));
    snprintf(u.name, sizeof(u.name), "udrag-virtual");
    u.id.bustype = BUS_USB; u.id.vendor = 0x1234; u.id.product = 0x5678;
    if (write(fd, &u, sizeof(u)) < 0) { perror("write dev"); return 1; }
    if (ioctl(fd, UI_DEV_CREATE) < 0) { perror("dev create"); return 1; }
    usleep(800000);
    /* park at a corner first so the walk to the source is deterministic */
    ev(fd, EV_REL, REL_X, 6000); ev(fd, EV_REL, REL_Y, 6000);
    ev(fd, EV_SYN, SYN_REPORT, 0);
    usleep(300000);
    walk(fd, x1, y1, 40);
    ev(fd, EV_KEY, BTN_LEFT, 1); ev(fd, EV_SYN, SYN_REPORT, 0);
    usleep(100000);
    walk(fd, x2, y2, 12); /* small steps: crosses the DnD drag threshold */
    usleep(hold * 1000);
    ev(fd, EV_KEY, BTN_LEFT, 0); ev(fd, EV_SYN, SYN_REPORT, 0);
    usleep(300000);
    ioctl(fd, UI_DEV_DESTROY);
    close(fd);
    return 0;
}
