# Choosing the host

One machine **hosts** tr3x: it runs Rex's server and `tr3x sync`, and every thread's pane is a
process there. Every other device is a **client**: it needs only the Rex app, attached to the
host. T3 runs the agents on whichever machine each project lives on; the host only draws them.

```
host (Mac or Linux)                       clients (any Rex app)
  Rex server + tr3x sync                   attach to the host's address
  T3 here, and T3 on other machines
  paired with it
```

Pick a machine that stays on. Agents on a machine stop when its own T3 quits; threads on the host
keep running while every client is closed.

## Make a machine the host

Run the installer on it (T3 must be running there; on Linux, `t3 service install` keeps it up):

```sh
curl -fsSL https://raw.githubusercontent.com/macnason/tr3x/master/install.sh | sh
```

- **Mac:** Rex is the app, and sync starts with it.
- **Linux:** the installer keeps Rex's server as a systemd user service, on your tailnet as its
  own HTTPS device (`<host>-rex`). The first time, approve that device in your Tailscale account;
  the installer prints the link, then the address. Run `loginctl enable-linger $USER` so it
  outlives your login.

## Attach a client

On the host, `rex server tailscale pair` shows a QR code for its tailnet address (the Linux
installer shows it once). In Rex on a phone or iPad, **Pair via QR Code** and scan it. On a Mac:
the globe at the bottom of the sidebar → **Add Remote Host** → the address it printed. The host
then shows in the sidebar and in **Filter Hosts**.

`rex hosts add` only saves an address; it doesn't attach the app. To check, run
`rex -S <address> client ls` and look for the app.

## Other machines that run T3

`tr3x pair` on the host finds them on your tailnet and pairs each (over SSH, or with one command
to run there). Their threads join the host's session, labelled with the machine's name.

## Move the host

1. **Old host:** `tr3x uninstall`. On Linux also `systemctl --user disable --now rex-server`,
   and remove its `-rex` device in Tailscale.
2. **New host:** run the installer, then `tr3x pair` for the machines the old host followed.
3. **Clients:** remove the old host (**Remove Host…**), add the new one.

A machine that stays a client must not run tr3x: a second install gives it a session of its
own, and its threads show twice, under **This Mac** and under the host.

## Pictures from another device

Rex carries text into a pane on another machine, but not pictures or files: a picture's ⌘V
arrives as a bare key, and a drop as nothing. Send pictures to the host with Taildrop instead:
Share → Tailscale → the host, on a Mac or a phone, or `tailscale file cp shot.png <host>:`. Sync
takes them in and attaches each to the Reply of the thread whose window is active, as if it had
been pasted. Files that aren't pictures are kept in tr3x's state folder, in `taildrop-kept`.
Taildrop must be on for your tailnet.

## When something looks wrong

- **Panes are bare shell prompts** after the host's Rex server restarted: close the host's
  **T3 Code** session (`rex -S <address> kill "T3 Code"`); sync rebuilds every pane. Closing
  every window at once settles nothing.
- **A client sees no host:** the app isn't attached. Add it in the app, then `client ls`.
- **Git over SSH fails on a headless host** that signs with a desktop key agent: use HTTPS
  (`gh auth setup-git`).
- On Linux, Reply's input sits between two rules, not on a fill: there's no Rex app to read a
  theme from.
