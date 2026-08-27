// The darwin canvas unit. Rust owns grids, damage and the ring state machine;
// this unit owns every Metal, CoreText and IOSurface call. Nothing here parses
// terminal bytes and nothing here touches AppKit windows.
#include <stdint.h>

typedef struct SoksakCanvas SoksakCanvas;

// NULL when no Metal device exists; the caller refuses by name — no fallback.
SoksakCanvas *soksak_canvas_create(void);
void soksak_canvas_free(SoksakCanvas *canvas);

// First-contact probe: paint a glyph grid into a fresh IOSurface of the given
// pixel size and report how many pixels received ink. 0 on success; a negative
// value names the failing stage.
int32_t soksak_canvas_spike(SoksakCanvas *canvas, uint32_t width, uint32_t height,
                            uint64_t *ink_pixels);

// Monospace cell geometry for a face at pt × scale device pixels.
typedef struct SoksakFontMetrics {
    double cellW;
    double cellH;
    double ascent;
} SoksakFontMetrics;
int32_t soksak_canvas_font_metrics(SoksakCanvas *canvas, const char *family, double pt,
                                   double scale, SoksakFontMetrics *out);

// Raster one codepoint's coverage at pt × scale into the caller's buffer
// (capW × capH bytes). Reports the placed bitmap and its baseline offsets.
typedef struct SoksakGlyphBitmap {
    uint32_t width;
    uint32_t height;
    int32_t left; // ink offset from the cell's left edge
    int32_t top;  // ink offset up from the baseline
} SoksakGlyphBitmap;
int32_t soksak_canvas_raster_glyph(SoksakCanvas *canvas, const char *family, double pt,
                                   double scale, uint32_t codepoint, uint8_t *coverage,
                                   uint32_t capW, uint32_t capH, SoksakGlyphBitmap *placed);
