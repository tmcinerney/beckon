#ifndef PANEL_SUPPORT_H
#define PANEL_SUPPORT_H
#include <stddef.h>
#include <sys/types.h>
typedef struct BPToken BPToken;
typedef struct { unsigned char *bytes; size_t length; int code; pid_t pending_pid; } BPResult;
BPToken *bp_token_create(void);
void bp_token_cancel(BPToken *);
void bp_token_destroy(BPToken *);
void bp_result_free(BPResult *);
BPResult bp_process(const char *executable, char *const argv[], char *const envp[], int milliseconds, BPToken *);
BPResult bp_socket(const char *path, const char *request, int milliseconds, BPToken *);
#endif
