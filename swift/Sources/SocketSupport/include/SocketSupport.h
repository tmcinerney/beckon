#ifndef SOCKET_SUPPORT_H
#define SOCKET_SUPPORT_H
#include <stddef.h>
typedef struct BPToken BPToken;
typedef struct { unsigned char *bytes; size_t length; int code; } BPResult;
BPToken *bp_token_create(void);
void bp_token_cancel(BPToken *);
void bp_token_destroy(BPToken *);
void bp_result_free(BPResult *);
BPResult bp_socket_limit(const char *, const char *, int, size_t, BPToken *);
#endif
