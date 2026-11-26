// palera1n's daemon job starts this instead of palera1n's daemon: it
// bootstraps the companion helper's launchd job, then runs palera1n's daemon
// as before. No network listener, package manager or disk command.
#include <errno.h>
#include <spawn.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>
extern char **environ;
int main(void) {
    pid_t pid = 0;
    char *const bootstrap[] = {"/cores/binpack/bin/launchctl", "bootstrap", "system",
        "/cores/binpack/Library/LaunchDaemons/org.orchardlinux.companion.plist", NULL};
    int result = posix_spawn(&pid, bootstrap[0], NULL, NULL, bootstrap, environ);
    if (!result) {
        int status = 0;
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        fprintf(stderr, "Orchard companion: launchd bootstrap status %d\n", status);
    } else {
        fprintf(stderr, "Orchard companion: launchd bootstrap failed: %d\n", result);
    }
    char *const original[] = {"/cores/binpack/usr/sbin/palera1nd", NULL};
    execve(original[0], original, environ);
    perror("Orchard companion: exec palera1nd");
    return 1;
}
