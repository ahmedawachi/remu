remu-relay for Windows
======================

remu-relay is the small server Remu desks use to find each other. Only one
person on the network runs it; everyone else just needs the address it prints.

Run it from a permanent folder, from PowerShell or by double-clicking it, and
keep its window open — closing the window stops the relay:

  .\remu-relay.exe

It normally prints the address to give everyone else, on a line starting
"LAN:". If it prints only a localhost address, use this PC's IP address
(ipconfig shows it) as ws://<that address>:8765.

If this build is not signed, SmartScreen says "Windows protected your PC" the
first time: click "More info", then "Run anyway".

The relay has to accept incoming connections, so Windows asks the first time
it starts. Allow it on the network type you are on and approve the
administrator prompt. On a standard account an administrator has to allow it
once, in PowerShell run as administrator:

  New-NetFirewallRule -DisplayName "Remu relay" -Direction Inbound -Action Allow `
    -Protocol TCP -LocalPort 8765 -Profile Domain,Private

The relay only introduces two desks to each other. It sees their IDs and
names, their network addresses, and the messages that set a session up. The
screen, keyboard, mouse, files and chat never pass through it: they travel
directly between the two machines, encrypted.

Keep it on your own network. The desk app connects to a relay over plain
ws://, not wss://, so a relay reachable from the internet would expose that
set-up information to anyone on the path; if it must be reachable, start it
with a token (remu-relay --token <secret>) and give every desk the same token
in Settings, and put it behind a VPN rather than on the open internet.
