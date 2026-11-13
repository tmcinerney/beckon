// Socket-only extraction of the prototype's PanelSupport helper.
#include "SocketSupport.h"
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/stat.h>
#include <time.h>

struct BPToken { atomic_int cancelled; };
BPToken *bp_token_create(void) { return calloc(1, sizeof(BPToken)); }
void bp_token_cancel(BPToken *t) { if(t) atomic_store(&t->cancelled, 1); }
void bp_token_destroy(BPToken *t) { free(t); }
void bp_result_free(BPResult *r) { free(r->bytes); r->bytes=NULL; r->length=0; }
static double now_ms(void) {
    struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec*1000 + (double)ts.tv_nsec/1000000;
}
// Codes: 0 complete line, 1 cancelled, 2 deadline, 3 output cap, 5 I/O or truncated line.
BPResult bp_socket_limit(const char *path,const char *request,int ms,size_t cap,BPToken *token) {
    BPResult r={0};r.code=5;
    struct stat st;
    if(cap==0 || cap>1048576 || !request || strlen(request)>4096 || !path || strlen(path)>=sizeof(((struct sockaddr_un*)0)->sun_path) || ms<100 || ms>2000
       || stat(path,&st)<0 || !S_ISSOCK(st.st_mode)) return r;
    int fd=socket(AF_UNIX,SOCK_STREAM,0);if(fd<0)return r;
    fcntl(fd,F_SETFD,FD_CLOEXEC);
    int yes=1;setsockopt(fd,SOL_SOCKET,SO_NOSIGPIPE,&yes,sizeof(yes));
    if(fcntl(fd,F_SETFL,fcntl(fd,F_GETFL)|O_NONBLOCK)<0){close(fd);return r;}
    struct sockaddr_un addr={0};addr.sun_family=AF_UNIX;addr.sun_len=sizeof(addr);
    strcpy(addr.sun_path,path);
    double deadline=now_ms()+ms;size_t sent=0,total=strlen(request),used=0;
    unsigned char *out=malloc(cap);if(!out){close(fd);return r;}
    int connected=connect(fd,(struct sockaddr*)&addr,sizeof(addr));
    if(connected<0 && errno!=EINPROGRESS && errno!=EAGAIN) goto done;
    for(;;){
        if(token && atomic_load(&token->cancelled)){r.code=1;break;}
        if(now_ms()>=deadline){r.code=2;break;}
        struct pollfd p={fd,sent<total?POLLOUT:POLLIN,0};
        int ready=poll(&p,1,20);if(ready<0 && errno!=EINTR)break;if(ready<=0)continue;
        int error=0;socklen_t len=sizeof(error);
        if(getsockopt(fd,SOL_SOCKET,SO_ERROR,&error,&len)<0 || error)break;
        if(sent<total){
            ssize_t n=send(fd,request+sent,total-sent,0);
            if(n>0)sent+=(size_t)n;else if(n<0&&errno!=EAGAIN&&errno!=EINTR)break;
        }else{
            unsigned char buf[8192];ssize_t n=read(fd,buf,sizeof(buf));
            if(n==0)break;
            if(n<0){if(errno==EAGAIN||errno==EINTR)continue;break;}
            for(ssize_t i=0;i<n;i++){
                if(used==cap){r.code=3;goto done;}
                out[used++]=buf[i];
                if(buf[i]=='\n'){r.code=0;goto done;}
            }
        }
    }
done:
    close(fd);r.bytes=out;r.length=used;return r;
}
