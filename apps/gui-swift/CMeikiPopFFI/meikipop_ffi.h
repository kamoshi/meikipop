#ifndef MEIKIPOP_FFI_H
#define MEIKIPOP_FFI_H

#include <stdbool.h>
#include <stdint.h>

typedef struct MeikiPopPipeline MeikiPopPipeline;

void meikipop_logging_init(void);

MeikiPopPipeline *meikipop_pipeline_start(
    const char *dictionary_path,
    char **error_out
);

char *meikipop_pipeline_poll(MeikiPopPipeline *pipeline);

void meikipop_pipeline_set_popup_bounds(
    MeikiPopPipeline *pipeline,
    bool visible,
    int32_t left,
    int32_t top,
    uint32_t width,
    uint32_t height
);

void meikipop_pipeline_destroy(MeikiPopPipeline *pipeline);
void meikipop_string_free(char *string);

#endif
