/* Synthesize real pointer input via uinput, closing the loop through
 * `hyprctl cursorpos` so display scaling doesn't matter.
 * usage: uiclick X Y N   — move to logical (X,Y), send N left clicks. */
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

int main(int argc, char **argv) {
    if (argc < 4) { fprintf(stderr, "usage: uiclick X Y N\n"); return 1; }
    int tx = atoi(argv[1]), ty = atoi(argv[2]), n = atoi(argv[3]);
    int fd = open("/dev/uinput", O_WRONLY | O_NONBLOCK);
    if (fd < 0) { perror("open uinput"); return 1; }
    ioctl(fd, UI_SET_EVBIT, EV_KEY);
    ioctl(fd, UI_SET_KEYBIT, BTN_LEFT);
    ioctl(fd, UI_SET_EVBIT, EV_REL);
    ioctl(fd, UI_SET_RELBIT, REL_X);
    ioctl(fd, UI_SET_RELBIT, REL_Y);
    struct uinput_user_dev u;
    memset(&u, 0, sizeof(u));
    snprintf(u.name, sizeof(u.name), "uiclick-virtual");
    u.id.bustype = BUS_USB; u.id.vendor = 0x1234; u.id.product = 0x5678;
    if (write(fd, &u, sizeof(u)) < 0) { perror("write dev"); return 1; }
    if (ioctl(fd, UI_DEV_CREATE) < 0) { perror("dev create"); return 1; }
    usleep(800000); /* let libinput pick the device up */
    /* park at a known corner, then walk to the target reading the real
       position between steps (immune to pointer acceleration/scaling) */
    ev(fd, EV_REL, REL_X, 6000); ev(fd, EV_REL, REL_Y, 6000);
    ev(fd, EV_SYN, SYN_REPORT, 0);
    usleep(300000);
    for (int i = 0; i < 6; i++) {
        int cx, cy;
        if (cursor_pos(&cx, &cy) < 0) break;
        int dx = tx - cx, dy = ty - cy;
        if (dx > -4 && dx < 4 && dy > -4 && dy < 4) break;
        ev(fd, EV_REL, REL_X, dx); ev(fd, EV_REL, REL_Y, dy);
        ev(fd, EV_SYN, SYN_REPORT, 0);
        usleep(200000);
    }
    for (int i = 0; i < n; i++) {
        ev(fd, EV_KEY, BTN_LEFT, 1); ev(fd, EV_SYN, SYN_REPORT, 0);
        usleep(60000);
        ev(fd, EV_KEY, BTN_LEFT, 0); ev(fd, EV_SYN, SYN_REPORT, 0);
        usleep(80000); /* inside the 400ms double-click window */
    }
    usleep(300000);
    ioctl(fd, UI_DEV_DESTROY);
    close(fd);
    return 0;
}
