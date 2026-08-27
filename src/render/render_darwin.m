// The darwin canvas unit — Metal, CoreText and IOSurface live here and only
// here. The probe below is unimplemented until its pixel test demands it.
#import "render_darwin.h"
#import <Metal/Metal.h>

struct SoksakCanvas {
    id<MTLDevice> device;
    id<MTLCommandQueue> queue;
};

SoksakCanvas *soksak_canvas_create(void) {
    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    if (device == nil) {
        return NULL;
    }
    id<MTLCommandQueue> queue = [device newCommandQueue];
    if (queue == nil) {
        return NULL;
    }
    SoksakCanvas *canvas = calloc(1, sizeof(SoksakCanvas));
    canvas->device = device;
    canvas->queue = queue;
    return canvas;
}

void soksak_canvas_free(SoksakCanvas *canvas) {
    if (canvas == NULL) {
        return;
    }
    canvas->device = nil;
    canvas->queue = nil;
    free(canvas);
}

int32_t soksak_canvas_spike(SoksakCanvas *canvas, uint32_t width, uint32_t height,
                            uint64_t *ink_pixels) {
    (void)canvas;
    (void)width;
    (void)height;
    (void)ink_pixels;
    return -1; // stage -1: the probe is not painted yet
}
