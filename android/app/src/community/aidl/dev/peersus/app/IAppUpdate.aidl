package dev.peersus.app;
/** One replacement of this application's own package. No caller-supplied package, user, path, or command. */
interface IAppUpdate {
    int begin(long size, boolean reopen) = 0;
    int write(in byte[] chunk) = 1;
    int finish() = 2;
    void destroy() = 16777114;
}
