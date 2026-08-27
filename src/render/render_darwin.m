// The darwin canvas unit — Metal, CoreText and IOSurface live here and only
// here. Rust owns grids, damage and the ring state machine; this unit paints
// what it is handed and reports pixels back. Nothing here parses terminal
// bytes and nothing here touches AppKit windows.
#import "render_darwin.h"
#import <Metal/Metal.h>
#import <CoreText/CoreText.h>
#import <CoreGraphics/CoreGraphics.h>
#import <IOSurface/IOSurfaceRef.h>

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

// One glyph mask tiled over every cell; coverage becomes ink on black.
static NSString *const kSpikeShader = @""
    "#include <metal_stdlib>\n"
    "using namespace metal;\n"
    "struct SpikeParams { uint cellW; uint cellH; uint glyphW; uint glyphH; };\n"
    "kernel void spikeCells(texture2d<float, access::write> out [[texture(0)]],\n"
    "                       const device uchar *mask [[buffer(0)]],\n"
    "                       constant SpikeParams &p [[buffer(1)]],\n"
    "                       uint2 gid [[thread_position_in_grid]]) {\n"
    "    if (gid.x >= out.get_width() || gid.y >= out.get_height()) { return; }\n"
    "    uint lx = gid.x % p.cellW;\n"
    "    uint ly = gid.y % p.cellH;\n"
    "    float cover = 0.0;\n"
    "    if (lx < p.glyphW && ly < p.glyphH) {\n"
    "        cover = float(mask[ly * p.glyphW + lx]) / 255.0;\n"
    "    }\n"
    "    out.write(float4(cover, cover, cover, 1.0), gid);\n"
    "}\n";

typedef struct {
    uint32_t cellW;
    uint32_t cellH;
    uint32_t glyphW;
    uint32_t glyphH;
} SpikeParams;

int32_t soksak_canvas_spike(SoksakCanvas *canvas, uint32_t width, uint32_t height,
                            uint64_t *ink_pixels) {
    if (canvas == NULL || ink_pixels == NULL || width == 0 || height == 0) {
        return -1;
    }
    @autoreleasepool {
        // Stage 2: the IOSurface the application would composite.
        NSDictionary *properties = @{
            (__bridge NSString *)kIOSurfaceWidth : @(width),
            (__bridge NSString *)kIOSurfaceHeight : @(height),
            (__bridge NSString *)kIOSurfaceBytesPerElement : @4,
            (__bridge NSString *)kIOSurfacePixelFormat : @((uint32_t)'BGRA'),
        };
        IOSurfaceRef surface = IOSurfaceCreate((__bridge CFDictionaryRef)properties);
        if (surface == NULL) {
            return -2;
        }

        // Stage 3: a texture view over the same pixels.
        MTLTextureDescriptor *descriptor = [MTLTextureDescriptor
            texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                         width:width
                                        height:height
                                     mipmapped:NO];
        descriptor.usage = MTLTextureUsageShaderWrite;
        id<MTLTexture> texture = [canvas->device newTextureWithDescriptor:descriptor
                                                                iosurface:surface
                                                                    plane:0];
        if (texture == nil) {
            CFRelease(surface);
            return -3;
        }

        // Stage 4: CoreText rasterizes one glyph into an 8-bit mask.
        const uint32_t glyphW = 16, glyphH = 32;
        uint8_t maskBytes[16 * 32] = {0};
        CTFontRef font = CTFontCreateWithName(CFSTR("Menlo"), 24.0, NULL);
        if (font == NULL) {
            CFRelease(surface);
            return -4;
        }
        UniChar character = 'A';
        CGGlyph glyph = 0;
        if (!CTFontGetGlyphsForCharacters(font, &character, &glyph, 1)) {
            CFRelease(font);
            CFRelease(surface);
            return -4;
        }
        CGColorSpaceRef gray = CGColorSpaceCreateDeviceGray();
        CGContextRef bitmap = CGBitmapContextCreate(maskBytes, glyphW, glyphH, 8, glyphW,
                                                    gray, (CGBitmapInfo)kCGImageAlphaNone);
        CGColorSpaceRelease(gray);
        if (bitmap == NULL) {
            CFRelease(font);
            CFRelease(surface);
            return -5;
        }
        CGContextSetGrayFillColor(bitmap, 1.0, 1.0);
        CGPoint position = CGPointMake(1.0, 8.0);
        CTFontDrawGlyphs(font, &glyph, &position, 1, bitmap);
        CGContextRelease(bitmap);
        CFRelease(font);

        // Stage 6/7: the compute pipeline, compiled from embedded source.
        NSError *error = nil;
        id<MTLLibrary> library = [canvas->device newLibraryWithSource:kSpikeShader
                                                              options:nil
                                                                error:&error];
        if (library == nil) {
            CFRelease(surface);
            return -6;
        }
        id<MTLFunction> function = [library newFunctionWithName:@"spikeCells"];
        id<MTLComputePipelineState> pipeline =
            function == nil ? nil
                            : [canvas->device newComputePipelineStateWithFunction:function
                                                                            error:&error];
        if (pipeline == nil) {
            CFRelease(surface);
            return -7;
        }

        // Stage 8: cell geometry and the mask travel as buffers.
        SpikeParams params = {.cellW = glyphW, .cellH = glyphH, .glyphW = glyphW, .glyphH = glyphH};
        id<MTLBuffer> maskBuffer = [canvas->device newBufferWithBytes:maskBytes
                                                               length:sizeof(maskBytes)
                                                              options:MTLResourceStorageModeShared];
        id<MTLBuffer> paramsBuffer = [canvas->device newBufferWithBytes:&params
                                                                 length:sizeof(params)
                                                                options:MTLResourceStorageModeShared];
        if (maskBuffer == nil || paramsBuffer == nil) {
            CFRelease(surface);
            return -8;
        }

        // Stage 9: one dispatch covers the whole grid.
        id<MTLCommandBuffer> commands = [canvas->queue commandBuffer];
        id<MTLComputeCommandEncoder> encoder = [commands computeCommandEncoder];
        if (commands == nil || encoder == nil) {
            CFRelease(surface);
            return -9;
        }
        [encoder setComputePipelineState:pipeline];
        [encoder setTexture:texture atIndex:0];
        [encoder setBuffer:maskBuffer offset:0 atIndex:0];
        [encoder setBuffer:paramsBuffer offset:0 atIndex:1];
        [encoder dispatchThreads:MTLSizeMake(width, height, 1)
            threadsPerThreadgroup:MTLSizeMake(8, 8, 1)];
        [encoder endEncoding];
        [commands commit];
        [commands waitUntilCompleted];

        // Stage 10: the verdict is read from the surface, not from the encoder.
        if (IOSurfaceLock(surface, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) {
            CFRelease(surface);
            return -10;
        }
        uint64_t ink = 0;
        const uint8_t *base = IOSurfaceGetBaseAddress(surface);
        size_t stride = IOSurfaceGetBytesPerRow(surface);
        for (uint32_t y = 0; y < height; y++) {
            const uint8_t *row = base + y * stride;
            for (uint32_t x = 0; x < width; x++) {
                const uint8_t *pixel = row + x * 4;
                if (pixel[0] > 8 || pixel[1] > 8 || pixel[2] > 8) {
                    ink++;
                }
            }
        }
        IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL);
        CFRelease(surface);
        *ink_pixels = ink;
        return 0;
    }
}
