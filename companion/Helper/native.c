#include <errno.h>
#include <unistd.h>
// The device's ECID from its device tree (IODeviceTree:/chosen
// unique-chip-id, 8 bytes little-endian). iOS ships IOKit without headers,
// so the few functions used are declared here.
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef mach_port_t io_object_t;
typedef io_object_t io_registry_entry_t;
extern io_registry_entry_t IORegistryEntryFromPath(mach_port_t, const char *);
extern CFTypeRef IORegistryEntryCreateCFProperty(io_registry_entry_t, CFStringRef, CFAllocatorRef, uint32_t);
extern kern_return_t IOObjectRelease(io_object_t);

int oc_ecid(uint64_t *out) {
    io_registry_entry_t chosen = IORegistryEntryFromPath(MACH_PORT_NULL, "IODeviceTree:/chosen");
    if (!chosen) return -1;
    CFTypeRef value = IORegistryEntryCreateCFProperty(chosen, CFSTR("unique-chip-id"), kCFAllocatorDefault, 0);
    IOObjectRelease(chosen);
    if (!value) return -1;
    int status = -1;
    if (CFGetTypeID(value) == CFDataGetTypeID() && CFDataGetLength(value) == 8) {
        uint8_t bytes[8];
        CFDataGetBytes(value, CFRangeMake(0, 8), bytes);
        uint64_t v = 0;
        for (int i = 7; i >= 0; i--) v = (v << 8) | bytes[i];
        *out = v;
        status = 0;
    }
    CFRelease(value);
    return status;
}

// --- Opening the app ---

#include <dlfcn.h>
#include <sys/sysctl.h>

// Whether a process whose name starts with `name` runs (process names are
// cut to 15 characters).
int oc_running(const char *name) {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_ALL, 0};
    size_t size = 0;
    if (sysctl(mib, 4, NULL, &size, NULL, 0) != 0 || size == 0) return 0;
    size += size / 4;
    struct kinfo_proc *procs = malloc(size);
    if (!procs) return 0;
    int found = 0;
    if (sysctl(mib, 4, procs, &size, NULL, 0) == 0) {
        size_t n = size / sizeof(struct kinfo_proc);
        size_t want = strlen(name) < 15 ? strlen(name) : 15;
        for (size_t i = 0; i < n && !found; i++) {
            if (strncmp(procs[i].kp_proc.p_comm, name, want) == 0) found = 1;
        }
    }
    free(procs);
    return found;
}

// SpringBoard's own launcher, the fallback when LaunchServices declines.
int oc_springboard_launch(const char *bundle_id) {
    void *sbs = dlopen("/System/Library/PrivateFrameworks/SpringBoardServices.framework/SpringBoardServices", RTLD_LAZY);
    if (!sbs) return -1;
    int (*launch)(CFStringRef, Boolean) = (int (*)(CFStringRef, Boolean))dlsym(sbs, "SBSLaunchApplicationWithIdentifier");
    if (!launch) return -1;
    CFStringRef id = CFStringCreateWithCString(kCFAllocatorDefault, bundle_id, kCFStringEncodingUTF8);
    int status = launch(id, false);
    CFRelease(id);
    return status;
}

/* Restarts the device: flush the file systems, then the kernel's reboot.
   Returns only on failure (errno). */
int oc_restart(void) {
    sync();
    return reboot(0) == 0 ? 0 : errno;
}
