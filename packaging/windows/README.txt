Remu for Windows
================

Move this folder somewhere permanent first — Documents, or C:\Tools — and run
remu.exe from there. Windows remembers the firewall decision below against
the exact path of the program, so running it from Downloads and moving it
later means being asked again, or silently blocked.

Nothing needs installing. Windows 10 (1803 or later) or Windows 11, 64-bit.

First run: SmartScreen
----------------------
This build carries no code-signing certificate, so Windows says "Windows
protected your PC". Click "More info", then "Run anyway". It only asks once.

First session: the firewall
---------------------------
The first time a session starts — right after you click Accept, or right
after you connect to someone — Windows asks whether Remu may use the network.

  Tick "Private networks", and "Public networks" too if your office network
  is not marked as private, then click "Allow access".

This is the step that decides whether the picture ever arrives. Do not click
Cancel: Windows then blocks Remu without ever asking again, and every later
session fails with no explanation. If that has already happened, open
"Windows Defender Firewall with Advanced Security", delete the inbound rules
named "remu", and run Remu again to get the question back.

Connecting
----------
Open Settings, put the relay address you were given into "Relay server URL",
click "Save settings", then "Reconnect relay". Your nine-digit desk ID appears
at the top right once the relay answers; the bottom bar shows the relay you
are pointed at and whether the connection is up.

To view another desk, type its nine digits under "Remote Desk" and press
Connect. The other machine has to accept before anything is shared.

Windows needs no screen-recording or accessibility permission. The one limit
is that Remu cannot type into windows running as administrator, or into the
UAC prompt, unless Remu itself was started as administrator.
