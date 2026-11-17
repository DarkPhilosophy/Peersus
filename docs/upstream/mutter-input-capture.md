# Two reports for Mutter's input capture (drafts, not filed)

Drafts for the owner to file at <https://gitlab.gnome.org/GNOME/mutter/-/issues>. Nothing here was sent anywhere.
What was observed and what was only read in the source is kept apart in each. The observations are the runs
recorded in `engineering-log.md`, entry E-057.

## 1. After `Release` the pointer stays on the barrier and the session is activated again at once

**Version.** Mutter 51.1 (Wayland session, one monitor of 1920x1080, scale 1), through
`org.freedesktop.portal.InputCapture` with a libei receiver.

**What was done.** A session with one pointer barrier on the whole left edge (`x = 0`, `y = 0..1080`) is
enabled. The pointer is pushed into the edge, `Activated` arrives, and the application calls `Release` with
`activation_id` and a `cursor_position` 25 pixels in from the edge.

**Expected.** The pointer is at `cursor_position`, off the barrier, and the session is activated again only
when the pointer is next pushed into the edge.

**Observed.** `Activated` arrives again 10 to 17 ms after every `Release`, with the `cursor_position` of the
first activation (the same `x = 0`, `y = 555.2310180664063` 231 times in 20 s), whichever position each
`Release` asked for. In one run this went on at about a hundred activations a second for 80 s while no input
device produced any event (every `/dev/input/event*` was read during the run). A pointer that is on the
barrier when the session is enabled also activates it with its first motion in any direction, away from the
edge included.

**What the application does about it.** It removes its barriers for 350 ms after a release and sets them
again; a session made that way is not activated again by a still pointer.

**Read in the source, not established as the cause.** `handle_release` (`meta-input-capture-session.c`) calls
`release_all_barriers`, then `meta_input_capture_session_deactivate`, then `clutter_seat_warp_pointer` with the
position truncated to integers. Why the pointer is then still found on the barrier was not found.

**Possibly the same.** Deskflow issues 10226 ("Release cursor_position hint appears not to be applied") and
10248, on GNOME 50.

## 2. A captured motion with no relative part is forwarded with amounts that were never set

**Version.** Mutter 51.1, `src/backends/meta-input-capture-session.c`,
`meta_input_capture_session_process_event`.

**Read in the source.**

```c
double dx, dy, dx_constrained, dy_constrained;
...
case CLUTTER_MOTION:
  ...
  clutter_event_get_relative_motion (event, &dx, &dy, NULL, NULL,
                                     &dx_constrained, &dy_constrained);

  eis_device_pointer_motion (session->eis_pointer,
                             dx - dx_constrained,
                             dy - dy_constrained);
```

The four variables are not initialised and the return value of `clutter_event_get_relative_motion` is not
looked at. For a motion event that carries no relative motion (a pointer placed by the compositor itself, as by
a warp) the function is understood to leave them as they were, so what is sent to the receiver is whatever
was on the stack.

**Observed.** A libei receiver of a capture session got pointer motions whose amounts were not numbers a hand
makes; one of them was not a number at all, and a position computed from it was refused further on. The
events came right after a `Release` with a `cursor_position`, which is when the compositor moves the pointer
by itself. Which event of the compositor each of them was is not known: that link is inferred from the source
above.

**Suggested.** Forward a motion only when `clutter_event_get_relative_motion` returns true, or set the
amounts to zero first.

**What the application does about it.** It drops captured motions whose amounts are not finite or are larger
than any device reports in one event.
