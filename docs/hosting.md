# Choosing the host

One machine **hosts** t3rex: it runs Rex's server and `t3rex sync`, and every thread's pane is a
process there. Every other device is a **client**: it needs only the Rex app, attached to the
host. T3 runs the agents on whichever machine each project lives on; the host only draws them.

```
host (Mac or Linux)                       clients (any Rex app)
  Rex server + t3rex sync                   attach to the host's address
  T3 here, and T3 on other machines
  paired with it
```

Pick a machine that stays on. Agents on a machine stop when its own T3 quits; threads on the host
keep running while every client is closed.

## Make a machine the host

Run the installer on it (T3 must be running there; on Linux, `t3 service install` keeps it up):

```sh
curl -fsSL https://raw.githubusercontent.com/macnason/t3rex/master/install.sh | sh
```

- **Mac:** Rex is the app, and sync starts with it.
- **Linux:** the installer keeps Rex's server as a systemd user service, on your tailnet as its
  own HTTPS device (`<host>-rex`). The first time, approve that device in your Tailscale account;
  the installer prints the link, then the address. Run `loginctl enable-linger $USER` so it
  outlives your login.

## Attach a client

In the Rex app on each device: the globe at the bottom of the sidebar → **Add Remote Host** → the
host's address. The host then shows in the sidebar and in **Filter Hosts**.

`rex hosts add` only saves an address; it doesn't attach the app. To check, run
`rex -S <address> client ls` and look for the app.

## Other machines that run T3

`t3rex pair` on the host finds them on your tailnet and pairs each (over SSH, or with one command
to run there). Their threads join the host's session, labelled with the machine's name.

## Move the host

1. **Old host:** `t3rex uninstall`. On Linux also `systemctl --user disable --now rex-server`,
   and remove its `-rex` device in Tailscale.
2. **New host:** run the installer, then `t3rex pair` for the machines the old host followed.
3. **Clients:** remove the old host (**Remove Host…**), add the new one.

A machine that stays a client must not run t3rex: a second install gives it a session of its
own, and its threads show twice, under **This Mac** and under the host.

## When something looks wrong

- **Panes are bare shell prompts** after the host's Rex server restarted: close the host's
  **T3 Code** session (`rex -S <address> kill "T3 Code"`); sync rebuilds every pane. Closing
  every window at once settles nothing.
- **A client sees no host:** the app isn't attached. Add it in the app, then `client ls`.
- **Git over SSH fails on a headless host** that signs with a desktop key agent: use HTTPS
  (`gh auth setup-git`).
- On Linux, Reply's input sits between two rules, not on a fill: there's no Rex app to read a
  theme from.
