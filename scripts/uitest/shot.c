/* shot — minimal wlr-screencopy client: grabs the (region of the) Wayland
 * output and writes it out as a PPM image, so the display path can be checked
 * objectively (no screenshot tooling is assumed on the test box).
 *
 * Build with ./build.sh. Usage:
 *   ./build/shot out.ppm [x y w h]   — the region is in LOGICAL pixels; the
 *     PPM comes back at the output's scale (see README.md). Artifacts belong in
 *     /tmp/videoplayer/, which build.sh creates.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>

#include "screencopy-client-protocol.h"

static struct wl_display *display;
static struct wl_registry *registry;
static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct zwlr_screencopy_manager_v1 *screencopy;
static struct wl_output *output;

static int frame_done;
static int frame_failed;
static int have_buffer;
static int img_w, img_h, img_stride;
static unsigned img_format;
static struct wl_buffer *wl_buffer;
static void *img_data;

static void frame_buffer(void *d, struct zwlr_screencopy_frame_v1 *frame,
			uint32_t format, uint32_t width, uint32_t height,
			uint32_t stride)
{
	(void) d; (void) frame;
	img_format = format;
	img_w = (int) width;
	img_h = (int) height;
	img_stride = (int) stride;

	size_t size = (size_t) stride * (size_t) height;
	int fd = memfd_create("shot", MFD_CLOEXEC);
	if (fd < 0) { perror("memfd_create"); exit(1); }
	if (ftruncate(fd, (off_t) size) < 0) { perror("ftruncate"); exit(1); }
	img_data = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (img_data == MAP_FAILED) { perror("mmap"); exit(1); }

	struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, (int32_t) size);
	wl_buffer = wl_shm_pool_create_buffer(pool, 0, img_w, img_h, img_stride,
					     img_format);
	wl_shm_pool_destroy(pool);
	close(fd);
	have_buffer = 1;
}

static void frame_ready(void *d, struct zwlr_screencopy_frame_v1 *frame,
			uint32_t tv_sec_hi, uint32_t tv_sec_lo, uint32_t tv_nsec)
{
	(void) d; (void) frame; (void) tv_sec_hi; (void) tv_sec_lo; (void) tv_nsec;
	frame_done = 1;
}

static void frame_failed_cb(void *d, struct zwlr_screencopy_frame_v1 *frame)
{
	(void) d; (void) frame;
	frame_failed = 1;
}

static void frame_flags(void *d, struct zwlr_screencopy_frame_v1 *frame,
			uint32_t flags)
{
	(void) d; (void) frame; (void) flags;
}
static void frame_damage(void *d, struct zwlr_screencopy_frame_v1 *frame,
			 uint32_t x, uint32_t y, uint32_t width, uint32_t height)
{
	(void) d; (void) frame; (void) x; (void) y; (void) width; (void) height;
}
static void frame_dmabuf(void *d, struct zwlr_screencopy_frame_v1 *frame,
			 uint32_t format, uint32_t width, uint32_t height)
{
	(void) d; (void) frame; (void) format; (void) width; (void) height;
}
static void frame_buffer_done(void *d, struct zwlr_screencopy_frame_v1 *frame)
{
	(void) d; (void) frame;
}

static const struct zwlr_screencopy_frame_v1_listener frame_listener = {
	.buffer = frame_buffer,
	.flags = frame_flags,
	.ready = frame_ready,
	.failed = frame_failed_cb,
	.damage = frame_damage,
	.linux_dmabuf = frame_dmabuf,
	.buffer_done = frame_buffer_done,
};

static void output_geometry(void *d, struct wl_output *o, int32_t x, int32_t y,
			    int32_t w, int32_t h, int32_t tf, const char *m,
			    const char *n, int32_t st)
{
	(void) d; (void) o; (void) x; (void) y; (void) w; (void) h;
	(void) tf; (void) m; (void) n; (void) st;
}
static void output_mode(void *d, struct wl_output *o, uint32_t f, int32_t w,
			int32_t h, int32_t r)
{
	(void) d; (void) o; (void) f; (void) w; (void) h; (void) r;
}
static void output_done(void *d, struct wl_output *o) { (void) d; (void) o; }
static void output_scale(void *d, struct wl_output *o, int32_t s)
{
	(void) d; (void) o; (void) s;
}
static void output_name(void *d, struct wl_output *o, const char *n)
{
	(void) d; (void) o; (void) n;
}
static void output_desc(void *d, struct wl_output *o, const char *n)
{
	(void) d; (void) o; (void) n;
}
static const struct wl_output_listener output_listener = {
	.geometry = output_geometry,
	.mode = output_mode,
	.done = output_done,
	.scale = output_scale,
	.name = output_name,
	.description = output_desc,
};

static void registry_global(void *d, struct wl_registry *r, uint32_t name,
			    const char *iface, uint32_t version)
{
	(void) d;
	if (strcmp(iface, wl_compositor_interface.name) == 0)
		compositor = wl_registry_bind(r, name, &wl_compositor_interface, 1);
	else if (strcmp(iface, wl_shm_interface.name) == 0)
		shm = wl_registry_bind(r, name, &wl_shm_interface, 1);
	else if (strcmp(iface, zwlr_screencopy_manager_v1_interface.name) == 0)
		screencopy = wl_registry_bind(r, name,
			&zwlr_screencopy_manager_v1_interface,
			version < 3 ? version : 3);
	else if (strcmp(iface, wl_output_interface.name) == 0 && !output) {
		output = wl_registry_bind(r, name, &wl_output_interface, 2);
		wl_output_add_listener(output, &output_listener, NULL);
	}
}
static void registry_global_remove(void *d, struct wl_registry *r, uint32_t n)
{
	(void) d; (void) r; (void) n;
}
static const struct wl_registry_listener registry_listener = {
	.global = registry_global,
	.global_remove = registry_global_remove,
};

int main(int argc, char **argv)
{
	const char *path = argc > 1 ? argv[1] : "/tmp/videoplayer/screen.ppm";
	int x = 0, y = 0, w = 0, h = 0;
	int region = argc >= 6;
	if (region) {
		x = atoi(argv[2]); y = atoi(argv[3]);
		w = atoi(argv[4]); h = atoi(argv[5]);
	}

	display = wl_display_connect(NULL);
	if (!display) {
		fprintf(stderr, "shot: cannot connect to the Wayland compositor\n");
		return 1;
	}
	registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, NULL);
	wl_display_roundtrip(display);

	if (!screencopy) {
		fprintf(stderr, "shot: compositor has no zwlr_screencopy_manager_v1\n");
		return 1;
	}
	if (!output || !shm || !compositor) {
		fprintf(stderr, "shot: missing wl_output/wl_shm/wl_compositor\n");
		return 1;
	}

	struct zwlr_screencopy_frame_v1 *frame;
	if (region)
		frame = zwlr_screencopy_manager_v1_capture_output_region(
			screencopy, 0, output, x, y, w, h);
	else
		frame = zwlr_screencopy_manager_v1_capture_output(screencopy, 0, output);
	zwlr_screencopy_frame_v1_add_listener(frame, &frame_listener, NULL);

	int copied = 0;
	while (!frame_done && !frame_failed) {
		if (have_buffer && !copied) {
			copied = 1;
			zwlr_screencopy_frame_v1_copy(frame, wl_buffer);
		}
		if (wl_display_dispatch(display) < 0)
			break;
	}

	if (frame_failed) {
		fprintf(stderr, "shot: capture failed\n");
		return 1;
	}

	FILE *f = fopen(path, "wb");
	if (!f) { perror(path); return 1; }
	if (img_format != WL_SHM_FORMAT_ARGB8888 && img_format != WL_SHM_FORMAT_XRGB8888) {
		fprintf(stderr, "shot: unexpected shm fourcc %#x (want XRGB8888)\n", img_format);
		return 1;
	}
	fprintf(f, "P6\n%d %d\n255\n", img_w, img_h);
	for (int row = 0; row < img_h; row++) {
		const unsigned char *src = (const unsigned char *) img_data + (size_t) row * img_stride;
		for (int col = 0; col < img_w; col++) {
			/* wl shm ARGB8888 is little-endian BGRA on disk -> RGB */
			fputc(src[col * 4 + 2], f);
			fputc(src[col * 4 + 1], f);
			fputc(src[col * 4 + 0], f);
		}
	}
	fclose(f);
	printf("shot: wrote %s (%dx%d, fourcc %#x)\n", path, img_w, img_h, img_format);
	(void) compositor;
	return 0;
}
