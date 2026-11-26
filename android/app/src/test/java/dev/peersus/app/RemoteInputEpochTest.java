package dev.peersus.app;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.List;

/** Exercise the actual replay queue fence without a phone or accessibility grant. */
public final class RemoteInputEpochTest {
    public static void main(String[] args) {
        RemoteInput.ReplayEpoch epoch = new RemoteInput.ReplayEpoch();
        ArrayDeque<Runnable> queue = new ArrayDeque<>();
        List<String> seen = new ArrayList<>();

        // Ordinary release must retire old work before a later withdrawal or replacement.
        epoch.enqueue(queue::addLast, ignored -> seen.add("old input"));
        epoch.fence(queue::addLast, () -> seen.add("released"));
        epoch.enqueue(queue::addLast, ignored -> seen.add("new input"));
        while (!queue.isEmpty()) queue.removeFirst().run();
        require(seen.equals(List.of("released", "new input")), seen);

        // A batch that had started must see the fence before its next side effect.
        seen.clear();
        epoch.enqueue(queue::addLast, ticket -> {
            if (epoch.current(ticket)) seen.add("already submitted");
            epoch.fence(queue::addLast, () -> seen.add("withdrawn"));
            if (epoch.current(ticket)) seen.add("late input");
        });
        while (!queue.isEmpty()) queue.removeFirst().run();
        require(seen.equals(List.of("already submitted", "withdrawn")), seen);
    }

    private static void require(boolean ok, List<String> seen) {
        if (!ok) throw new AssertionError("Unexpected replay order: " + seen);
    }
}
