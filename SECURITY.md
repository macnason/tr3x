# Security

tr3x can send messages to your coding agents and approve what they ask, on every machine it's
paired with. That's the job, so it's worth knowing exactly what it holds and who it talks to.

## What it holds

- **One token per machine**, from T3's own pairing, the way T3's apps pair. It lasts 30 days,
  renews itself, and shows in T3 → Settings → Connections as **Rex on** and this Mac's name,
  where you can revoke it. It asks for `orchestration:read`, `orchestration:operate`,
  `access:read`, `access:write`, `filesystem:read`, `source-control:write`, and
  `environment:maintain` to update T3 there when you ask it to.
- Tokens live in `~/.local/state/tr3x/credentials.json`, mode 0600. A token is only sent to a
  server whose descriptor names the machine it was issued for, checked on every connection.
- `tr3x env forget <machine>` unpairs a machine and revokes Rex's session there where it can.
  `tr3x uninstall` removes the Rex config and stops sync.

## Who it talks to

- **T3** on this Mac, over loopback, and on the machines you paired, at the addresses T3
  publishes for them.
- **Rex** on this Mac, through Rex's own CLI.
- **Rex hosts**, only when you pair through one: `rex -S <host> do` runs T3's CLI there, on
  Rex's own connection to that host. tr3x listens on no port of its own.
- **SSH**, only when you pair over it, with `BatchMode=yes`: it never prompts for a password.
  A tailnet host's first key is accepted, since Tailscale has already checked the machine.
- **GitHub**, for the install script's download, checked against the release's SHA-256;
  once a day for tr3x's latest release, which it installs the same way; and through your own
  `gh` login when you open a pull request drawer. The drawer can also download
  [sup](https://github.com/superultrainc/sup) from its releases, checked against their
  checksums, when you press Enter on its offer.
- **Taildrop**, while sync runs: `tailscale file get --wait --loop` receives files your own
  devices send over the tailnet, so a picture can reach a pane on another machine. A picture
  goes to the active thread's Reply as if pasted; anything else is kept aside and never sent.
- **Rex's tailnet device**, on a Linux host: the installer runs Rex's server as a systemd user
  service and, where Tailscale is there, as its own HTTPS device on your tailnet.

Nothing else of tr3x's own: no telemetry, no analytics, no other servers. The one exception is
what an agent's HTML preview asks for, below.

Updating itself means tr3x runs whatever this repository releases, as `install.sh` does. A
release is checked against its SHA-256 and must say the version it claims, but both come from
the same GitHub release. `tr3x updates notify` asks before installing one, and
`tr3x updates off` stops the daily check. T3 is only updated when you say so.

[How it works](docs/how-it-works.md#pairing-with-t3) covers pairing in full.

## HTML previews

When an agent shows a page with T3's `html_render` and Chrome or Chromium is installed, tr3x
opens that page in a headless browser to take the picture shown in the conversation. The page
is the agent's: its scripts run and it can fetch whatever it names, fonts, images, scripts or
data from any address, as it would in a browser tab. That includes addresses only this machine
can reach, such as its own services, your local network and your tailnet, and a request can
carry what the page puts in it. tr3x adds no filter, since previews that load a chart library
or a web font are what they're for.

What bounds it: the browser starts with an empty profile of its own each time, so it has none
of your cookies or logins; the page is a local file, with the browser's usual limits on reading
others; it holds none of tr3x's tokens; one capture runs at a time, and each is stopped after
30 seconds. It only reads pages T3 stored on this machine.

If you don't want a thread's pages loaded unattended, don't install Chrome or Chromium for
tr3x: without one, previews show as links. `TR3X_CHROME` points
tr3x at a different browser, for instance one in a sandbox or on a network with limited
access. `tr3x doctor` says whether it found one.

## Reporting a problem

Please report a vulnerability privately, through **Security → Report a vulnerability** on the
GitHub repository, rather than in an issue. Say what you found, how to reproduce it, and which
versions of tr3x (`tr3x --version`), T3 and Rex you saw it with.
