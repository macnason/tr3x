# Security

t3rex can send messages to your coding agents and approve what they ask, on every machine it's
paired with. That's the job, so it's worth knowing exactly what it holds and who it talks to.

## What it holds

- **One token per machine**, from T3's own pairing, the way T3's apps pair. It lasts 30 days,
  renews itself, and shows in T3 → Settings → Connections as **Rex on** and this Mac's name,
  where you can revoke it. It asks for `orchestration:read`, `orchestration:operate`,
  `access:read`, `access:write`, `filesystem:read` and `source-control:write`.
- Tokens live in `~/.local/state/t3rex/credentials.json`, mode 0600. A token is only sent to a
  server whose descriptor names the machine it was issued for, checked on every connection.
- `t3rex env forget <machine>` unpairs a machine and revokes Rex's session there where it can.
  `t3rex uninstall` removes the Rex config and stops sync.

## Who it talks to

- **T3** on this Mac, over loopback, and on the machines you paired, at the addresses T3
  publishes for them.
- **Rex** on this Mac, through Rex's own CLI.
- **Joining**: while sync runs, it takes `curl -fsSL http://<this Mac>:3779/join | sh` from
  other machines. It listens on this Mac's tailnet addresses only, answers only machines that
  `tailscale whois` says belong to the same person, and trades the code with T3 at the
  machine's tailnet address, never one the request names.
- **SSH**, only when you pair over it, with `BatchMode=yes`: it never prompts for a password.
  A tailnet host's first key is accepted, since Tailscale has already checked the machine.
- **GitHub**, for the install script's download, checked against the release's SHA-256, and
  through your own `gh` login when you open a pull request drawer.

Nothing else. No telemetry, no analytics, no other servers.

[How it works](docs/how-it-works.md#pairing-with-t3) covers pairing in full.

## Reporting a problem

Please report a vulnerability privately, through **Security → Report a vulnerability** on the
GitHub repository, rather than in an issue. Say what you found, how to reproduce it, and which
versions of t3rex (`t3rex --version`), T3 and Rex you saw it with.
