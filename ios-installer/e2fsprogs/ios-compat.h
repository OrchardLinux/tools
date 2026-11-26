/* Installer-lab portability shim, not a general fork emulation.
 * These optional subprocess features are deliberately unsupported on iOS.
 * Include system declarations first so their availability attributes do not
 * attach to our replacements. Core filesystem operations are unchanged. */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <errno.h>
#include <stdint.h>
#include <sys/ioctl.h>
/* iPhoneOS SDK omits sys/disk.h. Use the same XNU ABI as disk-probe so
 * libext2fs can size partition devices without unaligned read probing. */
#ifndef DKIOCGETBLOCKSIZE
#define DKIOCGETBLOCKSIZE _IOR('d', 24, uint32_t)
#endif
#ifndef DKIOCGETBLOCKCOUNT
#define DKIOCGETBLOCKCOUNT _IOR('d', 25, uint64_t)
#endif
static inline int lab_system(const char *s) { (void)s; errno = ENOTSUP; return -1; }
static inline FILE *lab_popen(const char *s, const char *m) {
    (void)s; (void)m; errno = ENOTSUP; return NULL;
}
static inline int lab_pclose(FILE *f) { (void)f; errno = ENOTSUP; return -1; }
static inline pid_t lab_fork(void) { errno = ENOTSUP; return -1; }
static inline int lab_daemon(int a, int b) { (void)a; (void)b; errno = ENOTSUP; return -1; }
static inline int lab_execlp(const char *p, const char *a, ...) {
    (void)p; (void)a; errno = ENOTSUP; return -1;
}
static inline int lab_getentropy(void *buf, size_t n) {
    if (n > 256) { errno = EIO; return -1; }
    arc4random_buf(buf, n);
    return 0;
}
#define getentropy lab_getentropy
#define system lab_system
#define popen lab_popen
#define pclose lab_pclose
#define fork lab_fork
#define daemon lab_daemon
#define execlp lab_execlp
