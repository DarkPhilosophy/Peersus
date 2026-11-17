# Local dependency backport

Baseline: netwatch 0.19.3, upstream `https://github.com/n0-computer/net-tools`, revision `0030e57fc70981895bb86212ff0f059cc41c8a4e`. Original MIT and Apache-2.0 license files are retained. Imported from the published crates.io source on 2026-11-04.

Change: backport only the Linux/Android netlink dependency versions used by netwatch 0.20.0, revision `80b449c74d38c564c598ae286de13032276f3515`: packet-core 0.9, packet-route 0.33, proto 0.13 and sys 0.9. Match its netdev 0.46.3 dependency on all desktop targets as well; the older Windows-only netdev requirement otherwise retains the obsolete netlink set in the cross-target lockfile. The Linux/Android netlink implementation files in the two published releases are identical. Keep the existing 0.19.3 public interface expected by the transport and portmapper; no source implementation is replaced.

Purpose: eliminate the unmaintained `paste` dependency from the network-monitoring chain (RUSTSEC-2024-0436), without suppressing the advisory or changing the application's transport interface. Other independent consumers of `paste` must be migrated separately.

Removal: drop this patch when the transport and portmapper accept a released netwatch using the maintained netlink set. Validate network-monitor tests, transport tests, Linux and Android builds, and the strict advisory audit after removal. Source/compile checks alone do not prove device connectivity.

2026-11-09: replace the five deprecated noq-udp `send` calls with `try_send`. This intentionally preserves I/O errors for the existing retry/rebind/error handlers instead of silently accepting failed sends. It is a transport behavior change, not a warning suppression; verify loopback and real-device connectivity after updates.
