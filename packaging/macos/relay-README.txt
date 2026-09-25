remu-relay for macOS
====================

remu-relay is the small server Remu desks use to find each other. Only one
person on the network runs it; everyone else just needs the address it prints.

macOS quarantines anything a browser downloads, and this binary is not
notarised, so clear the quarantine once after unpacking, then start it from
Terminal and keep that window open — closing it stops the relay:

  xattr -c ./remu-relay
  ./remu-relay

It normally prints the address to give everyone else, on a line starting
"LAN:". If it prints only a localhost address, use this Mac's IP address from
System Settings > Network instead, as ws://<that address>:8765. If the macOS
firewall is on, allow remu-relay to accept incoming connections when asked.

The relay only introduces two desks to each other. It sees their IDs and
names, their network addresses, and the messages that set a session up. The
screen, keyboard, mouse, files and chat never pass through it: they travel
directly between the two machines, encrypted.

Keep it on your own network. The desk app connects to a relay over plain
ws://, not wss://, so a relay reachable from the internet would expose that
set-up information to anyone on the path; if it must be reachable, start it
with a token (./remu-relay --token <secret>) and give every desk the same token
in Settings, and put it behind a VPN rather than on the open internet.
