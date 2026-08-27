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
    id<MTLComputePipelineState> cellPipeline;
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
    canvas->cellPipeline = nil;
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

// One CTFont per (family, pt × scale) for the life of the process; every pane
// and every glyph of that face shares it.
static CTFontRef soksakFontFor(const char *family, double pt, double scale, bool *exact) {
    static NSMutableDictionary<NSString *, id> *cache;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ cache = [NSMutableDictionary new]; });
    NSString *name = [NSString stringWithUTF8String:family];
    if (name == nil) {
        return NULL;
    }
    double px = pt * scale;
    NSString *key = [NSString stringWithFormat:@"%@/%.3f", name, px];
    @synchronized(cache) {
        id held = cache[key];
        if (held != nil) {
            *exact = true;
            return (__bridge CTFontRef)held;
        }
    }
    CTFontRef font = CTFontCreateWithName((__bridge CFStringRef)name, px, NULL);
    if (font == NULL) {
        return NULL;
    }
    // CoreText substitutes a default face for unknown names; a substituted
    // family is a refusal here, not a fallback (P5).
    NSString *resolved = CFBridgingRelease(CTFontCopyFamilyName(font));
    NSString *postscript = CFBridgingRelease(CTFontCopyPostScriptName(font));
    bool matches = [resolved caseInsensitiveCompare:name] == NSOrderedSame ||
                   [postscript caseInsensitiveCompare:name] == NSOrderedSame ||
                   [postscript hasPrefix:[name stringByReplacingOccurrencesOfString:@" "
                                                                         withString:@""]];
    if (!matches) {
        CFRelease(font);
        *exact = false;
        return NULL;
    }
    @synchronized(cache) {
        // Two threads can miss together; the first stored font wins and the
        // loser's copy is released here — never the winner's, which callers
        // already hold.
        id held = cache[key];
        if (held != nil) {
            CFRelease(font);
            *exact = true;
            return (__bridge CTFontRef)held;
        }
        cache[key] = (__bridge_transfer id)font;
        *exact = true;
        return (__bridge CTFontRef)cache[key];
    }
}

int32_t soksak_canvas_font_metrics(SoksakCanvas *canvas, const char *family, double pt,
                                   double scale, SoksakFontMetrics *out) {
    if (canvas == NULL || family == NULL || out == NULL || pt <= 0 || scale <= 0) {
        return -1;
    }
    @autoreleasepool {
        bool exact = false;
        CTFontRef font = soksakFontFor(family, pt, scale, &exact);
        if (font == NULL) {
            return exact ? -2 : -3; // -3: the face is unknown on this host
        }
        UniChar reference = 'M';
        CGGlyph glyph = 0;
        if (!CTFontGetGlyphsForCharacters(font, &reference, &glyph, 1)) {
            return -4;
        }
        CGSize advance = CGSizeZero;
        CTFontGetAdvancesForGlyphs(font, kCTFontOrientationHorizontal, &glyph, &advance, 1);
        double ascent = CTFontGetAscent(font);
        double descent = CTFontGetDescent(font);
        double leading = CTFontGetLeading(font);
        out->cellW = advance.width;
        out->cellH = ceil(ascent + descent + leading);
        out->ascent = ascent;
        return 0;
    }
}

int32_t soksak_canvas_raster_glyph(SoksakCanvas *canvas, const char *family, double pt,
                                   double scale, uint32_t codepoint, uint8_t *coverage,
                                   uint32_t capW, uint32_t capH, SoksakGlyphBitmap *placed) {
    if (canvas == NULL || family == NULL || coverage == NULL || placed == NULL) {
        return -1;
    }
    @autoreleasepool {
        bool exact = false;
        CTFontRef font = soksakFontFor(family, pt, scale, &exact);
        if (font == NULL) {
            return exact ? -2 : -3;
        }
        UniChar units[2];
        CFIndex count = 0;
        if (codepoint <= 0xFFFF) {
            units[0] = (UniChar)codepoint;
            count = 1;
        } else {
            uint32_t value = codepoint - 0x10000;
            units[0] = (UniChar)(0xD800 + (value >> 10));
            units[1] = (UniChar)(0xDC00 + (value & 0x3FF));
            count = 2;
        }
        CGGlyph glyphs[2] = {0, 0};
        CTFontRef face = font;
        bool ownsFace = false;
        if (!CTFontGetGlyphsForCharacters(font, units, glyphs, count) || glyphs[0] == 0) {
            // The primary face has no glyph here; CoreText names the face the
            // system would substitute, and the cell metrics stay primary.
            CFStringRef text = CFStringCreateWithCharacters(NULL, units, count);
            if (text == NULL) {
                return -4;
            }
            CTFontRef fallback = CTFontCreateForString(font, text, CFRangeMake(0, count));
            CFRelease(text);
            if (fallback == NULL) {
                return -4;
            }
            if (!CTFontGetGlyphsForCharacters(fallback, units, glyphs, count) || glyphs[0] == 0) {
                CFRelease(fallback);
                return -4;
            }
            face = fallback;
            ownsFace = true;
        }
        int32_t result = 0;
        CGGlyph glyph = glyphs[0];
        CGRect bounds = CTFontGetBoundingRectsForGlyphs(face, kCTFontOrientationHorizontal,
                                                        &glyph, NULL, 1);
        uint32_t width = (uint32_t)ceil(bounds.size.width) + 2;
        uint32_t height = (uint32_t)ceil(bounds.size.height) + 2;
        if (width > capW || height > capH) {
            result = -5;
        } else {
            memset(coverage, 0, (size_t)capW * capH);
            CGColorSpaceRef gray = CGColorSpaceCreateDeviceGray();
            CGContextRef bitmap = CGBitmapContextCreate(coverage, width, height, 8, capW, gray,
                                                        (CGBitmapInfo)kCGImageAlphaNone);
            CGColorSpaceRelease(gray);
            if (bitmap == NULL) {
                result = -6;
            } else {
                CGContextSetGrayFillColor(bitmap, 1.0, 1.0);
                CGPoint position = CGPointMake(-bounds.origin.x + 1.0, -bounds.origin.y + 1.0);
                CTFontDrawGlyphs(face, &glyph, &position, 1, bitmap);
                CGContextRelease(bitmap);
                placed->width = width;
                placed->height = height;
                placed->left = (int32_t)floor(bounds.origin.x) - 1;
                placed->top = (int32_t)ceil(bounds.origin.y + bounds.size.height) + 1;
            }
        }
        if (ownsFace) {
            CFRelease(face);
        }
        return result;
    }
}

struct SoksakAtlas {
    id<MTLTexture> texture;
};

struct SoksakSurface {
    IOSurfaceRef surface;
    id<MTLTexture> texture;
    uint32_t width;
    uint32_t height;
};

SoksakAtlas *soksak_canvas_atlas_create(SoksakCanvas *canvas, uint32_t size) {
    if (canvas == NULL || size == 0) {
        return NULL;
    }
    MTLTextureDescriptor *descriptor = [MTLTextureDescriptor
        texture2DDescriptorWithPixelFormat:MTLPixelFormatR8Unorm
                                     width:size
                                    height:size
                                 mipmapped:NO];
    descriptor.usage = MTLTextureUsageShaderRead;
    id<MTLTexture> texture = [canvas->device newTextureWithDescriptor:descriptor];
    if (texture == nil) {
        return NULL;
    }
    SoksakAtlas *atlas = calloc(1, sizeof(SoksakAtlas));
    atlas->texture = texture;
    return atlas;
}

void soksak_canvas_atlas_free(SoksakAtlas *atlas) {
    if (atlas == NULL) {
        return;
    }
    atlas->texture = nil;
    free(atlas);
}

int32_t soksak_canvas_atlas_upload(SoksakAtlas *atlas, uint32_t x, uint32_t y, uint32_t w,
                                   uint32_t h, const uint8_t *coverage, uint32_t stride) {
    if (atlas == NULL || coverage == NULL || w == 0 || h == 0) {
        return -1;
    }
    if (x + w > atlas->texture.width || y + h > atlas->texture.height) {
        return -2;
    }
    [atlas->texture replaceRegion:MTLRegionMake2D(x, y, w, h)
                      mipmapLevel:0
                        withBytes:coverage
                      bytesPerRow:stride];
    return 0;
}

SoksakSurface *soksak_canvas_surface_create(SoksakCanvas *canvas, uint32_t width,
                                            uint32_t height) {
    if (canvas == NULL || width == 0 || height == 0) {
        return NULL;
    }
    NSDictionary *properties = @{
        (__bridge NSString *)kIOSurfaceWidth : @(width),
        (__bridge NSString *)kIOSurfaceHeight : @(height),
        (__bridge NSString *)kIOSurfaceBytesPerElement : @4,
        (__bridge NSString *)kIOSurfacePixelFormat : @((uint32_t)'BGRA'),
    };
    IOSurfaceRef ioSurface = IOSurfaceCreate((__bridge CFDictionaryRef)properties);
    if (ioSurface == NULL) {
        return NULL;
    }
    MTLTextureDescriptor *descriptor = [MTLTextureDescriptor
        texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                     width:width
                                    height:height
                                 mipmapped:NO];
    descriptor.usage = MTLTextureUsageShaderWrite;
    id<MTLTexture> texture = [canvas->device newTextureWithDescriptor:descriptor
                                                            iosurface:ioSurface
                                                                plane:0];
    if (texture == nil) {
        CFRelease(ioSurface);
        return NULL;
    }
    SoksakSurface *surface = calloc(1, sizeof(SoksakSurface));
    surface->surface = ioSurface;
    surface->texture = texture;
    surface->width = width;
    surface->height = height;
    return surface;
}

void soksak_canvas_surface_free(SoksakSurface *surface) {
    if (surface == NULL) {
        return;
    }
    surface->texture = nil;
    if (surface->surface != NULL) {
        CFRelease(surface->surface);
    }
    free(surface);
}

// One thread per pixel of the dirty band: paint the cell background, mix glyph
// coverage over it, then the underline and strikeout bands.
static NSString *const kCellShader = @""
    "#include <metal_stdlib>\n"
    "using namespace metal;\n"
    "struct Cell { ushort gx, gy, gw, gh; short inkL, inkT;\n"
    "              uint fg, bg, flags, link, reserved; };\n"
    "struct Params { uint cols; uint cellW; uint cellH; uint rowStart; uint rowCount; };\n"
    "static float4 unpackColor(uint value) {\n"
    "    return float4(float((value >> 16) & 255u), float((value >> 8) & 255u),\n"
    "                  float(value & 255u), float((value >> 24) & 255u)) / 255.0;\n"
    "}\n"
    "kernel void paintCells(texture2d<float, access::write> out [[texture(0)]],\n"
    "                       texture2d<float, access::read> atlas [[texture(1)]],\n"
    "                       const device Cell *cells [[buffer(0)]],\n"
    "                       constant Params &p [[buffer(1)]],\n"
    "                       uint2 gid [[thread_position_in_grid]]) {\n"
    "    if (gid.x >= p.cols * p.cellW || gid.y >= p.rowCount * p.cellH) { return; }\n"
    "    uint absY = p.rowStart * p.cellH + gid.y;\n"
    "    if (gid.x >= out.get_width() || absY >= out.get_height()) { return; }\n"
    "    uint cx = gid.x / p.cellW;\n"
    "    uint cy = p.rowStart + gid.y / p.cellH;\n"
    "    Cell cell = cells[cy * p.cols + cx];\n"
    "    float4 color = unpackColor(cell.bg);\n"
    "    int lx = int(gid.x % p.cellW) - int(cell.inkL);\n"
    "    int ly = int(gid.y % p.cellH) - int(cell.inkT);\n"
    "    if (cell.gw > 0 && lx >= 0 && lx < int(cell.gw) && ly >= 0 && ly < int(cell.gh)) {\n"
    "        float coverage = atlas.read(uint2(uint(int(cell.gx) + lx), uint(int(cell.gy) + ly))).r;\n"
    "        color = mix(color, unpackColor(cell.fg), coverage);\n"
    "    }\n"
    "    uint yy = gid.y % p.cellH;\n"
    "    if ((cell.flags & 1u) != 0u && yy >= p.cellH - 2u) { color = unpackColor(cell.fg); }\n"
    "    if ((cell.flags & 2u) != 0u && yy == p.cellH / 2u) { color = unpackColor(cell.fg); }\n"
    "    out.write(color, uint2(gid.x, absY));\n"
    "}\n";

typedef struct {
    uint32_t cols;
    uint32_t cellW;
    uint32_t cellH;
    uint32_t rowStart;
    uint32_t rowCount;
} SoksakPaintParams;

int32_t soksak_canvas_paint(SoksakCanvas *canvas, SoksakAtlas *atlas, SoksakSurface *surface,
                            const void *cells, uint32_t cols, uint32_t rows, uint32_t cellW,
                            uint32_t cellH, uint32_t rowStart, uint32_t rowCount) {
    if (canvas == NULL || atlas == NULL || surface == NULL || cells == NULL) {
        return -1;
    }
    if (cols == 0 || rows == 0 || cellW == 0 || cellH == 0 || rowCount == 0 ||
        rowStart + rowCount > rows) {
        return -2;
    }
    @autoreleasepool {
        if (canvas->cellPipeline == nil) {
            NSError *error = nil;
            id<MTLLibrary> library = [canvas->device newLibraryWithSource:kCellShader
                                                                  options:nil
                                                                    error:&error];
            if (library == nil) {
                return -3;
            }
            id<MTLFunction> function = [library newFunctionWithName:@"paintCells"];
            if (function != nil) {
                canvas->cellPipeline =
                    [canvas->device newComputePipelineStateWithFunction:function error:&error];
            }
            if (canvas->cellPipeline == nil) {
                return -4;
            }
        }
        id<MTLBuffer> cellBuffer =
            [canvas->device newBufferWithBytes:cells
                                        length:(NSUInteger)cols * rows * 32
                                       options:MTLResourceStorageModeShared];
        SoksakPaintParams params = {
            .cols = cols, .cellW = cellW, .cellH = cellH, .rowStart = rowStart,
            .rowCount = rowCount,
        };
        id<MTLBuffer> paramsBuffer = [canvas->device newBufferWithBytes:&params
                                                                 length:sizeof(params)
                                                                options:MTLResourceStorageModeShared];
        if (cellBuffer == nil || paramsBuffer == nil) {
            return -5;
        }
        id<MTLCommandBuffer> commands = [canvas->queue commandBuffer];
        id<MTLComputeCommandEncoder> encoder = [commands computeCommandEncoder];
        if (commands == nil || encoder == nil) {
            return -6;
        }
        [encoder setComputePipelineState:canvas->cellPipeline];
        [encoder setTexture:surface->texture atIndex:0];
        [encoder setTexture:atlas->texture atIndex:1];
        [encoder setBuffer:cellBuffer offset:0 atIndex:0];
        [encoder setBuffer:paramsBuffer offset:0 atIndex:1];
        [encoder dispatchThreads:MTLSizeMake(cols * cellW, rowCount * cellH, 1)
            threadsPerThreadgroup:MTLSizeMake(8, 8, 1)];
        [encoder endEncoding];
        [commands commit];
        [commands waitUntilCompleted];
        return 0;
    }
}

int32_t soksak_canvas_surface_read(SoksakSurface *surface, uint8_t *bgra, uint64_t cap) {
    if (surface == NULL || bgra == NULL) {
        return -1;
    }
    uint64_t needed = (uint64_t)surface->width * surface->height * 4;
    if (cap < needed) {
        return -2;
    }
    if (IOSurfaceLock(surface->surface, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) {
        return -3;
    }
    const uint8_t *base = IOSurfaceGetBaseAddress(surface->surface);
    size_t stride = IOSurfaceGetBytesPerRow(surface->surface);
    for (uint32_t y = 0; y < surface->height; y++) {
        memcpy(bgra + (uint64_t)y * surface->width * 4, base + (uint64_t)y * stride,
               (size_t)surface->width * 4);
    }
    IOSurfaceUnlock(surface->surface, kIOSurfaceLockReadOnly, NULL);
    return 0;
}

uint32_t soksak_canvas_surface_mach_port(SoksakSurface *surface) {
    if (surface == NULL || surface->surface == NULL) {
        return 0;
    }
    return (uint32_t)IOSurfaceCreateMachPort(surface->surface);
}
