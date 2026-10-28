# Hosting

[Design summary](README.md) · How to run Riftward for other people to play: from your own machine through a tunnel, or on a small server of its own.

Riftward is one program and a SQLite file. It listens for telnet on a TCP port (4000 by default), reads the world from `assets/`, and keeps everything that changes during play in `mud.db`. It needs very little: a few hundred MB of memory and a sliver of a CPU.

## The basics, wherever it runs

- **Build for release.** `cargo run --release` from the project's root folder. The release build is much faster than the development one, and running from the root is how the server finds `assets/`.
- **Settings are environment variables:** `MUD_ADDR` is where it listens (default `0.0.0.0:4000`, every network interface), and `MUD_DB` the database file (default `mud.db` in the root folder).
- **Make your own account first.** The first account created on a new database becomes an admin, so create yours before telling anyone the address.
- **Run the live server from its own copy of the repository,** not the one you develop in. Builder commands (`desc`, `dig`, `link`...) rewrite the zone files in `assets/zones/` as they're used, comments and formatting and all, and in a development folder those rewrites end up mixed into your own changes. A second clone (`git clone ... riftward-live`) keeps the live world's edits apart: commit them from there on purpose, or throw them away.
- **Back up `mud.db`.** It's the players' characters, items and the world's saved state. SQLite can copy itself safely while the server is running:

  ```sh
  sqlite3 mud.db ".backup 'backups/mud-$(date +%F).db'"
  ```

  Once a day is plenty to start with; a cron job can do it.
- **Updating:** pull the new code, then stop and restart the server (`cargo run --release` builds the new version first). Players are disconnected and mobs reset to their spawns; characters, items and the world's flags are kept, since they're in `mud.db`. Zone and data files (`assets/`) reload by themselves while it runs.
- **Telnet isn't encrypted.** Passwords travel in plain text, as on every classic MUD. Tell players not to reuse a password they care about.
- **Connecting:** any MUD client (Mudlet, TinTin++, MUSHclient) or plain `telnet <host> <port>`.

## From your own machine, through playit.gg

[playit.gg](https://playit.gg) gives a server running at home a public address without opening ports on your router: a small agent on your machine keeps an outbound connection to playit, and players connect to an address playit gives you, which it forwards to your server. It carries raw TCP, which telnet needs (many free tunnels only carry web traffic).

1. **Run the server listening only on your own machine,** since the tunnel is the way in:

   ```sh
   MUD_ADDR=127.0.0.1:4000 cargo run --release
   ```

2. **Install the playit agent** from playit.gg and sign in; it walks you through linking it to your account.
3. **Add a TCP tunnel** in playit's dashboard that points at local address `127.0.0.1`, port `4000`.
4. **Give players the address playit shows for the tunnel:** a host name and a port, something like `name.gl.at.ply.gg:12345`. Try it yourself first: `telnet <that host> <that port>`.

- **The game is up only while your machine is,** with both the server and the agent running. Fine for playtesting with friends; for a world that runs all the time, see below.
- **Free and paid:** what the free tier includes (how many tunnels, whether the address stays the same) can change; check playit's current terms.

## On a server of its own

For a world that keeps running when you're away, so rats breed and break out, and spawns come back on their own, a small virtual server is the cheapest always-on choice: the smallest instances from hosts such as Hetzner, DigitalOcean or Vultr are plenty. (Oracle's free tier would do too, when it has room, which it rarely does.)

1. **Install Rust and git** on it, and clone the repository into its own folder, as above.
2. **Open the port** (4000, or whichever `MUD_ADDR` uses) in the host's firewall, and leave `MUD_ADDR` at its default `0.0.0.0:4000` so it listens on the public address.
3. **Keep it running with systemd,** so it starts at boot and restarts if it stops. A unit file, `/etc/systemd/system/riftward.service`, for a user `mud` with the repository in `/home/mud/riftward`:

   ```ini
   [Unit]
   Description=Riftward MUD
   After=network.target

   [Service]
   User=mud
   WorkingDirectory=/home/mud/riftward
   Environment=MUD_ADDR=0.0.0.0:4000
   ExecStart=/home/mud/.cargo/bin/cargo run --release
   Restart=on-failure

   [Install]
   WantedBy=multi-user.target
   ```

   Then `sudo systemctl enable --now riftward`; `journalctl -u riftward -f` shows its log.
4. **Back up** with the `sqlite3 .backup` line above in the `mud` user's crontab, and copy the backups somewhere off the server now and then.

## Open questions

- [ ] **Where the public world lives,** once there's one: its own clone and database on a server, with builders' zone edits committed back from there?
- [ ] **A domain name** for it, rather than a tunnel's address or an IP.
