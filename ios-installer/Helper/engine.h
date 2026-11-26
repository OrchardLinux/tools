// The installer's engine (crates/inapp, src/ffi.rs), as the helper calls it.
#include <stddef.h>
char *oi_start(const char *config, void (*foreign)(const char *effect));
char *oi_request(const char *request);
void oi_storage(const char *snapshot);
void oi_split_started(const char *operation);
void oi_split_refused(const char *message);
void oi_log(const char *line);
void oi_free(char *text);
