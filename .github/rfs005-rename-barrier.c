/* Test apparatus: pause one owned coordinator's real state rename, not product code. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int is_coordinator(void) {
    char args[8192];
    FILE *f = fopen("/proc/self/cmdline", "r");
    if (!f) return 0;
    size_t n = fread(args, 1, sizeof(args) - 1, f);
    fclose(f);
    args[n] = 0;
    for (size_t i = 0; i < n; i += strlen(args + i) + 1)
        if (!strcmp(args + i, "coordinator")) return 1;
    return 0;
}

int rename(const char *from, const char *to) {
    static int claimed = 0;
    int (*real_rename)(const char *, const char *) = dlsym(RTLD_NEXT, "rename");
    const char *dir = getenv("RFS005_RECEIPT_DIR");
    const char *mode = getenv("RFS005_MODE");
    char arm[4096], waiting[4096], release[4096], done[4096];
    int pause = 0;
    if (dir && mode && !strcmp(mode, "barrier") && !claimed &&
        strlen(to) >= 11 && !strcmp(to + strlen(to) - 11, "/state.json") && is_coordinator()) {
        snprintf(arm, sizeof(arm), "%s/arm", dir);
        if (access(arm, F_OK) == 0) {
            claimed = 1;
            pause = 1;
            snprintf(waiting, sizeof(waiting), "%s/writer-waiting", dir);
            snprintf(release, sizeof(release), "%s/release-writer", dir);
            snprintf(done, sizeof(done), "%s/writer-completed", dir);
            FILE *f = fopen(waiting, "w");
            if (f) { fprintf(f, "%d\n%s\n%s\n", getpid(), from, to); fclose(f); }
            for (int i = 0; i < 15000 && access(release, F_OK); i++) usleep(1000);
        }
    }
    int result = real_rename(from, to);
    int saved_errno = errno;
    if (pause) {
        FILE *f = fopen(done, "w");
        if (f) { fprintf(f, "pid=%d result=%d errno=%d released=%d\n", getpid(), result,
                         saved_errno, access(release, F_OK) == 0); fclose(f); }
    }
    errno = saved_errno;
    return result;
}
