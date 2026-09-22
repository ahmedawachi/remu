Remu for macOS
==============

Drag Remu.app to the Applications folder, then open it from there.

First open
----------
This build carries no Apple developer signature, so macOS refuses the first
launch with "Apple could not verify Remu is free of malware", offering only
Done or Move to Trash.

To open it anyway:

  1. Double-click Remu once and dismiss the refusal.
  2. Open System Settings > Privacy & Security and scroll down. There is now a
     line about Remu having been blocked, with an "Open Anyway" button.
  3. Click it, authenticate, and confirm "Open Anyway" once more.

macOS remembers the decision and every later launch is normal. Control-clicking
and choosing Open is not a way around this on macOS 15 or newer, whatever older
advice says. The one-line equivalent, if you would rather use a terminal:

  xattr -dr com.apple.quarantine /Applications/Remu.app

Permissions
-----------
Remu needs two permissions, and only on the machine whose screen is being
shared. Neither is needed to view someone else's screen.

  Screen Recording   to capture the screen
  Accessibility      to let the other side use your keyboard and mouse

Grant them in System Settings > Privacy & Security, then quit and reopen Remu.
macOS only hands screen-recording permission to an application on its next
launch, so the restart is required rather than optional. Both permissions are
tied to where the app lives, so grant them after moving it to Applications,
not before.

Connecting
----------
Open Settings, put the relay address you were given into "Relay server URL",
click "Save settings", then "Reconnect relay". Your nine-digit desk ID appears
at the top right once the relay answers; the bottom bar shows the relay you are
pointed at and whether the connection is up.

To view another desk, type its nine digits under "Remote Desk" and press
Connect. The other machine has to accept before anything is shared.

remu-relay
----------
The disk image also carries remu-relay, the server two desks use to find each
other. Only one person on the network runs it:

  ./remu-relay

It prints the address to give everyone else. Nothing about a session travels
through it in readable form.
