# RustScale netstack patch provenance

Source: `https://github.com/rajsinghtech/rustscale`

Base revision: `8511e0b78074bf07b59d53cf1a2eb349cd0d2407`

Package: `crates/netstack`, version `0.1.3`, BSD-3-Clause.

Spurfire carries this focused patch until the upstream fix is released:

- Give `UdpListener` the netstack poll-loop `Notify` handle.
- Wake the poll loop after an application UDP datagram is enqueued.
- Make the UDP echo regression test use the production one-second idle fallback and require delivery within 500 ms.

Without the wake, the application send channel is not a `poll_loop` select arm and gameplay datagrams can batch behind the one-second fallback. Tracked upstream as <https://github.com/rajsinghtech/rustscale/issues/75>.
