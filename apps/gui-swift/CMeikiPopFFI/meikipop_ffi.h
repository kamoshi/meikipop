#ifndef MEIKIPOP_FFI_H
#define MEIKIPOP_FFI_H

#include <stdbool.h>

typedef struct MeikiPopPipeline MeikiPopPipeline;

char *meikipop_displays_json(char **error_out);

MeikiPopPipeline *meikipop_pipeline_start(
    const char *dictionary_path,
    char **error_out
);

char *meikipop_pipeline_poll(MeikiPopPipeline *pipeline);

void meikipop_pipeline_set_popup_visible(
    MeikiPopPipeline *pipeline,
    bool visible
);

void meikipop_pipeline_destroy(MeikiPopPipeline *pipeline);
void meikipop_string_free(char *string);

#endif
