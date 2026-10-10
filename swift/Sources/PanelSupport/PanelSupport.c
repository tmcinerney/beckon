#include "PanelSupport.h"
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <poll.h>
#include <spawn.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>

#define OUT_CAP 1048576
#define ERR_CAP 65536
struct BPToken { atomic_int cancelled; };
BPToken *bp_token_create(void) { return calloc(1, sizeof(BPToken)); }
void bp_token_cancel(BPToken *t) { if(t) atomic_store(&t->cancelled, 1); }
void bp_token_destroy(BPToken *t) { free(t); }
static int cancelled(BPToken *t) { return t && atomic_load(&t->cancelled); }
void bp_result_free(BPResult *r) { free(r->bytes); r->bytes=NULL; r->length=0; }
static double now_ms(void) {
    struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec*1000 + (double)ts.tv_nsec/1000000;
}
static void nonblock(int fd) { fcntl(fd, F_SETFL, fcntl(fd,F_GETFL)|O_NONBLOCK); }
static int group_gone(pid_t pid) { return kill(-pid,0)<0 && errno==ESRCH; }
static void close_fd(int *fd) { if(*fd>=0) close(*fd); *fd=-1; }
static int drain(int *fd, unsigned char *out, size_t *length, size_t cap) {
    unsigned char buf[8192];
    for(int chunk=0;chunk<32;chunk++) {
        ssize_t n=read(*fd,buf,sizeof(buf));
        if(n==0) { close_fd(fd); return 0; }
        if(n<0) { if(errno==EAGAIN || errno==EINTR) return 0; return 5; }
        if(*length+(size_t)n>cap) return 3;
        memcpy(out+*length,buf,(size_t)n); *length+=(size_t)n;
    }
    return 0;
}
// Codes: 0 success, 1 cancelled, 2 deadline, 3 output cap, 4 cleanup unconfirmed, 5 I/O, 6 exit.
BPResult bp_process(const char *exe,char *const argv[],char *const envp[],int ms,BPToken *token) {
    BPResult r={0}; r.code=5;
    if(!exe || exe[0]!='/' || ms<100 || ms>3000) return r;
    double start=now_ms(), deadline=start+ms;
    double reserve=ms<2500 ? ms*0.4 : 1000;
    int op[2]={-1,-1},ep[2]={-1,-1};
    unsigned char *errors=malloc(ERR_CAP), *output=malloc(OUT_CAP);
    if(!errors || !output || pipe(op)<0 || pipe(ep)<0) goto before_spawn;
    for(int i=0;i<2;i++){fcntl(op[i],F_SETFD,FD_CLOEXEC);fcntl(ep[i],F_SETFD,FD_CLOEXEC);}
    posix_spawn_file_actions_t actions;posix_spawnattr_t attr;
    if(posix_spawn_file_actions_init(&actions)) goto before_spawn;
    if(posix_spawnattr_init(&attr)) {posix_spawn_file_actions_destroy(&actions);goto before_spawn;}
    int setup=0;
    setup|=posix_spawn_file_actions_addopen(&actions,STDIN_FILENO,"/dev/null",O_RDONLY,0);
    setup|=posix_spawn_file_actions_adddup2(&actions,op[1],STDOUT_FILENO);
    setup|=posix_spawn_file_actions_adddup2(&actions,ep[1],STDERR_FILENO);
    setup|=posix_spawn_file_actions_addclose(&actions,op[0]);
    setup|=posix_spawn_file_actions_addclose(&actions,ep[0]);
    setup|=posix_spawn_file_actions_addclose(&actions,op[1]);
    setup|=posix_spawn_file_actions_addclose(&actions,ep[1]);
    setup|=posix_spawnattr_setflags(&attr,POSIX_SPAWN_SETPGROUP|POSIX_SPAWN_CLOEXEC_DEFAULT);
    setup|=posix_spawnattr_setpgroup(&attr,0);
    if(setup) {posix_spawn_file_actions_destroy(&actions);posix_spawnattr_destroy(&attr);goto before_spawn;}
    pid_t pid=-1;
    int spawned=cancelled(token) ? ECANCELED : posix_spawn(&pid,exe,&actions,&attr,argv,envp);
    posix_spawn_file_actions_destroy(&actions);posix_spawnattr_destroy(&attr);
    if(spawned) { r.code=spawned==ECANCELED ? 1 : 5; goto before_spawn; }
    close_fd(&op[1]);close_fd(&ep[1]);nonblock(op[0]);nonblock(ep[0]);
    size_t out_count=0,err_count=0; int status=0,reaped=0;
    r.code=0;
    for(;;) {
        if(cancelled(token)) {r.code=1;break;}
        if(now_ms()>=deadline-reserve){r.code=2;break;}
        if(op[0]>=0){int e=drain(&op[0],output,&out_count,OUT_CAP);if(e){r.code=e;break;}}
        if(ep[0]>=0){int e=drain(&ep[0],errors,&err_count,ERR_CAP);if(e){r.code=e;break;}}
        if(!reaped) {
            pid_t waited=waitpid(pid,&status,WNOHANG);
            if(waited==pid) reaped=1;
            else if(waited<0 && errno!=EINTR){r.code=5;break;}
        }
        if(reaped && op[0]<0 && ep[0]<0) { r.code=WIFEXITED(status)&&WEXITSTATUS(status)==0 ? 0 : 6; break; }
        if(reaped) {
            // AIDEV-NOTE: Drain buffered bytes, but terminate inherited children retaining an idle pipe.
            struct pollfd remaining[2]={{op[0],POLLIN,0},{ep[0],POLLIN,0}};
            if(poll(remaining,2,0)==0) {r.code=5;break;}
        }
        // A surviving pipe owner must not outlive this operation indefinitely.
        struct pollfd fds[2]={{op[0],POLLIN,0},{ep[0],POLLIN,0}};
        poll(fds,2,10);
    }
    // AIDEV-NOTE: Redirected descendants can survive EOF; success also owns group cleanup.
    kill(-pid,SIGTERM);
    double grace=now_ms()+250; if(grace>deadline-50) grace=deadline-50;
    while(now_ms()<grace) {
        if(!reaped) {
            pid_t w=waitpid(pid,&status,WNOHANG);
            if(w==pid || (w<0&&errno==ECHILD)) reaped=1;
        }
        if(reaped && group_gone(pid)) break;
        usleep(10000);
    }
    if(!reaped || !group_gone(pid)) kill(-pid,SIGKILL);
    close_fd(&op[0]);close_fd(&ep[0]);
    while(now_ms()<deadline) {
        if(!reaped) {
            pid_t w=waitpid(pid,&status,WNOHANG);
            if(w==pid || (w<0&&errno==ECHILD)) reaped=1;
        }
        if(reaped && group_gone(pid)) break;
        usleep(10000);
    }
    if(!reaped || !group_gone(pid)){r.code=4;r.pending_pid=pid;}
    r.bytes=output;r.length=out_count;free(errors);return r;
before_spawn:
    close_fd(&op[0]);close_fd(&op[1]);close_fd(&ep[0]);close_fd(&ep[1]);
    free(errors);free(output);return r;
}
BPResult bp_socket(const char *path,const char *request,int ms,BPToken *token) {
    BPResult r={0};r.code=5;
    struct stat st;
    if(!path || strlen(path)>=sizeof(((struct sockaddr_un*)0)->sun_path) || ms<100 || ms>2000
       || stat(path,&st)<0 || !S_ISSOCK(st.st_mode)) return r;
    int fd=socket(AF_UNIX,SOCK_STREAM,0);if(fd<0)return r;
    fcntl(fd,F_SETFD,FD_CLOEXEC);
    int yes=1;setsockopt(fd,SOL_SOCKET,SO_NOSIGPIPE,&yes,sizeof(yes));nonblock(fd);
    struct sockaddr_un addr={0};addr.sun_family=AF_UNIX;addr.sun_len=sizeof(addr);
    strcpy(addr.sun_path,path);
    double deadline=now_ms()+ms;size_t sent=0,total=strlen(request),used=0;
    unsigned char *out=malloc(OUT_CAP);if(!out){close(fd);return r;}
    int connected=connect(fd,(struct sockaddr*)&addr,sizeof(addr));
    if(connected<0 && errno!=EINPROGRESS && errno!=EAGAIN) goto done;
    for(;;){
        if(cancelled(token)){r.code=1;break;}
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
                if(used==OUT_CAP){r.code=3;goto done;}
                out[used++]=buf[i];
                if(buf[i]=='\n'){r.code=0;goto done;}
            }
        }
    }
done:
    close(fd);r.bytes=out;r.length=used;return r;
}
