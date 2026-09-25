Remu for Windows
================

Needs Windows 10 version 2004 (the May 2020 Update) or later, or Windows 11,
on a 64-bit Intel or AMD PC. Nothing needs installing.

Move this folder somewhere permanent first — Documents, or C:\Tools — and run
remu.exe from there. Windows remembers the firewall decision below against the
exact path of the program, so running it from Downloads and moving it later
means being asked again.

First run: SmartScreen
----------------------
If this build is not signed, Windows says "Windows protected your PC" the
first time. Click "More info", then "Run anyway". It only asks once.

First session: the firewall
---------------------------
The first time a session starts — right after you click Accept, or right
after you connect to someone — Windows asks whether Remu may use the network.
This step decides whether the picture ever arrives.

On an administrator account: tick the network types Remu should work on
("Private networks", and "Public networks" if your office network is listed as
public), click "Allow access", and approve the administrator prompt that
follows. Do not click Cancel — Windows then blocks Remu without asking again.

On a standard account, Windows blocks Remu whatever you click, and an
administrator has to allow it once. In PowerShell run as administrator, with
the path to your copy of remu.exe:

  New-NetFirewallRule -DisplayName "Remu" -Direction Inbound -Action Allow `
    -Program "C:\Tools\Remu\remu.exe" -Profile Domain,Private

Add ",Public" to the profile list if Windows lists your network as public.

If Remu was blocked by mistake, the same administrator PowerShell removes every
rule Windows made for it, so the question comes back on the next session:

  Get-NetFirewallApplicationFilter -Program "*\remu.exe" |
    Get-NetFirewallRule | Remove-NetFirewallRule

Connecting
----------
Open Settings, put the relay address you were given into "Relay server URL"
and click "Save settings". Your nine-digit desk ID appears at the top right
once the relay answers; the bottom bar shows the relay you are pointed at and
whether the connection is up.

To view another desk, type its nine digits under "Remote Desk" and press
Connect. The other machine has to accept before anything is shared.

Windows needs no screen-recording or accessibility permission. Two limits:
Remu cannot type into windows running as administrator unless Remu itself was
started as administrator, and nobody can see or answer a UAC prompt remotely —
someone at the machine has to click it.
