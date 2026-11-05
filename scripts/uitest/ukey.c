/* Type an ASCII string through uinput (EV_KEY, no modifiers).
 * usage: ukey "text"  — a-z, 0-9, '.', '-' */
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

static int key_code(char c) {
    if (c >= 'a' && c <= 'z') return KEY_A + (c - 'a');
    if (c >= 'A' && c <= 'Z') return KEY_A + (c - 'A');
    if (c >= '0' && c <= '9') return KEY_0 + (c - '0');
    if (c == '.') return KEY_DOT;
    if (c == '-') return KEY_MINUS;
    if (c == '!') return KEY_ESC;
    return -1;
}

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: ukey \"text\"\n"); return 1; }
    int fd = open("/dev/uinput", O_WRONLY | O_NONBLOCK);
    if (fd < 0) { perror("open uinput"); return 1; }
    ioctl(fd, UI_SET_EVBIT, EV_KEY);
    for (int k = KEY_0; k <= KEY_Z; k++) ioctl(fd, UI_SET_KEYBIT, k);
    struct uinput_user_dev u;
    memset(&u, 0, sizeof(u));
    snprintf(u.name, sizeof(u.name), "ukey-virtual");
    u.id.bustype = BUS_USB; u.id.vendor = 0x1234; u.id.product = 0x5678;
    if (write(fd, &u, sizeof(u)) < 0) { perror("write dev"); return 1; }
    if (ioctl(fd, UI_DEV_CREATE) < 0) { perror("dev create"); return 1; }
    usleep(800000);
    for (const char *c = argv[1]; *c; c++) {
        int code = key_code(*c);
        if (code < 0) continue;
        ev(fd, EV_KEY, code, 1); ev(fd, EV_SYN, SYN_REPORT, 0);
        usleep(150000);
        ev(fd, EV_KEY, code, 0); ev(fd, EV_SYN, SYN_REPORT, 0);
        usleep(250000);
    }
    usleep(300000);
    ioctl(fd, UI_DEV_DESTROY);
    close(fd);
    return 0;
}
