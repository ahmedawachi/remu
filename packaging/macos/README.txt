Remu for macOS
==============

Needs macOS 13.1 or later, on an Apple silicon or Intel Mac.

Drag Remu.app onto the Applications folder, then open it from there. On a
standard (non-administrator) account the Applications folder asks for an
administrator's password; dragging Remu into an "Applications" folder in your
home folder instead works without one.

First open
----------
If this build is not notarised by Apple, macOS refuses the first launch with
"Apple could not verify Remu is free of malware" or "Remu can't be opened".
To open it anyway:

  1. Double-click Remu once and dismiss the refusal.
  2. Open System Settings > Privacy & Security and scroll down. There is now a
     line about Remu having been blocked, with an "Open Anyway" button.
  3. Click it, authenticate, and confirm "Open Anyway" once more.

macOS remembers that for this version of Remu. Control-clicking and choosing
Open no longer works on macOS 15 or newer. The equivalent in a terminal:

  xattr -cr /Applications/Remu.app

Local network
-------------
On macOS 15 and later, the first time Remu connects to the relay, macOS asks
whether it may "find devices on local networks". Click Allow. If you click
Don't Allow, Remu cannot reach a relay or another desk on your network at all;
turn it back on under System Settings > Privacy & Security > Local Network.

Permissions
-----------
Only the Mac whose screen is being shared needs these two. Viewing someone
else's screen needs neither.

  Screen Recording   to capture this screen
  Accessibility      to let the other side use this keyboard and mouse

In Remu, open Settings and scroll to System permissions. Click "Request screen
recording" and allow Remu in the list that opens, then do the same with
"Request accessibility". Then quit Remu (Remu > Quit, or Cmd-Q) and open it
again: macOS only hands screen recording to an app when it next starts. The
two status pills turn green once each permission is in effect.

Both permissions, and the Open Anyway decision, belong to this exact build.
After installing a newer version, macOS may still show Remu switched on in
both lists while no longer honouring it. Select Remu in each list, remove it
with the minus button, then request both again from Remu's Settings.

Connecting
----------
Open Settings, put the relay address you were given into "Relay server URL"
and click "Save settings". Your nine-digit desk ID appears at the top right
once the relay answers; the bottom bar shows the relay you are pointed at and
whether the connection is up.

To view another desk, type its nine digits under "Remote Desk" and press
Connect. The other machine has to accept before anything is shared.

remu-relay
----------
The disk image also carries remu-relay, the small server two desks use to find
each other. Only one person on the network runs it. Copy it out of the disk
image, clear the download quarantine macOS puts on it, and start it:

  cp /Volumes/Remu/remu-relay ~/
  xattr -c ~/remu-relay
  ~/remu-relay

It normally prints the address to give everyone else, on a line starting
"LAN:". If it prints only a localhost address, use this Mac's IP address from
System Settings > Network instead, as ws://<that address>:8765. If the macOS
firewall is on, allow remu-relay to accept incoming connections when asked.

The relay only introduces two desks to each other. It sees their IDs and
names, their network addresses, and the messages that set a session up. The
screen, keyboard, mouse, files and chat never pass through it: they travel
directly between the two machines, encrypted.
