/* Compiled only into community APKs, never into the owner or the Play interpreter. */
#include "host.c"
#include <jni.h>
#include <sys/stat.h>

static int native_worker(void) {
    char name[256];
    int fd = open("/proc/self/cmdline", O_RDONLY | O_CLOEXEC);
    if (fd < 0) return 0;
    ssize_t length = read(fd, name, sizeof(name) - 1);
    close(fd);
    if (length <= 0) return 0;
    name[length] = 0;
    const char *slot = strrchr(name, ':');
    return slot && strncmp(slot, ":native_", 8) == 0 && slot[8] != 0
        && strspn(slot + 8, "0123456789") == strlen(slot + 8);
}

JNIEXPORT jint JNICALL Java_dev_peersus_app_NativePluginService_nativeRun(
    JNIEnv *env, jclass type, jstring module, jstring control, jstring data, jstring token
) {
    (void)type;
    if (!native_worker() || !module || !control || !data || !token) return 1;
    const char *m = (*env)->GetStringUTFChars(env, module, NULL);
    const char *c = m ? (*env)->GetStringUTFChars(env, control, NULL) : NULL;
    const char *d = c ? (*env)->GetStringUTFChars(env, data, NULL) : NULL;
    const char *t = d ? (*env)->GetStringUTFChars(env, token, NULL) : NULL;
    int result = 1;
    struct stat metadata;
    if (t && lstat(m, &metadata) == 0 && S_ISREG(metadata.st_mode) && metadata.st_uid == getuid()
            && (metadata.st_mode & 0222) == 0 && metadata.st_size > 0 && metadata.st_size <= 8 * 1024 * 1024) {
        result = peersus_native_run(m, c, d, t);
    }
    if (t) (*env)->ReleaseStringUTFChars(env, token, t);
    if (d) (*env)->ReleaseStringUTFChars(env, data, d);
    if (c) (*env)->ReleaseStringUTFChars(env, control, c);
    if (m) (*env)->ReleaseStringUTFChars(env, module, m);
    return result;
}
