package dev.peersus.app;
/** Read-only system counters Android closes to an application. No caller-supplied path, user or command. */
interface IReadings {
    /** The first line of /proc/stat (the ticks of all processors together), or null. */
    String processorTime() = 0;
    void destroy() = 16777114;
}
