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
